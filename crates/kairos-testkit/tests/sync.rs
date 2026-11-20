use futures::FutureExt;
use kairos_core::{
    Db,
    calendar::events::{load_series, upsert_event},
    db::settings::{get_meta, set_meta},
    http::api_v1::list_calendar,
    ports::Value,
    sched::queue::{process_queue, queue_length},
    sources::{
        model::{SourceRow, get_source},
        sync::{
            MAX_ICS_BYTES, SyncSummary, pick_due_source, send_source_alerts, sync_due_source,
            sync_source,
        },
    },
};
use kairos_testkit::{TestHub, ms};

const FEED: &str = "https://feeds.test/a.ics";
const BARK: &str = "https://bark.test/push";
const TRACE: &str = "consecutive_failures = consecutive_failures + 1";
const SINGLE: &[u8] = include_bytes!("../fixtures/timed-tzid.ics");
const OVERRIDE: &str = include_str!("../fixtures/override.ics");
const CANCELLED: &str = include_str!("../fixtures/cancelled-instance.ics");

fn hub() -> TestHub {
    let h = TestHub::new();
    h.db.exec(
        "INSERT INTO sources (id, name, url, color) VALUES (1, 'Teaching', ?1, '#aabbcc')",
        &[Value::Text(FEED.into())],
    )
    .unwrap();
    h
}

fn src(h: &TestHub) -> SourceRow {
    get_source(&h.db, 1).unwrap().unwrap()
}

fn install_bark(h: &TestHub) {
    h.sql(
        r#"INSERT INTO channels (kind, config) VALUES
       ('bark', '{"server":"https://bark.test","device_key":"device"}')"#,
    );
}

async fn sync_with(h: &TestHub, body: &[u8]) -> SyncSummary {
    h.http().respond(FEED, 200, &[], body);
    sync_source(h, &src(h), h.now()).await.unwrap()
}

async fn drain(h: &TestHub) {
    let result = process_queue(h, h.now()).await;
    assert_eq!(result.failed, 0);
    assert_eq!(result.step_error, None);
    assert_eq!(queue_length(&h.db).unwrap(), 0);
}

#[tokio::test]
async fn first_sync_imports_and_enqueues() {
    let h = hub();
    h.http()
        .respond(FEED, 200, &[("eTaG", "\"first\"")], SINGLE);
    let outcome = sync_due_source(&h, h.now()).await;
    assert_eq!(
        (outcome.ok, outcome.failed, outcome.step_error),
        (1, 0, None)
    );
    let rows = h.sql("SELECT title, source_id, origin FROM events");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].text("title").unwrap(), "Meeting, review");
    assert_eq!(rows[0].i64("source_id").unwrap(), 1);
    assert_eq!(rows[0].text("origin").unwrap(), "ics");
    assert_eq!(
        h.sql("SELECT reason FROM reconcile_queue")[0]
            .text("reason")
            .unwrap(),
        "sync"
    );
    assert_eq!(src(&h).last_result.as_deref(), Some("ok: 1 events"));
    assert_eq!(src(&h).etag.as_deref(), Some("\"first\""));
    assert_eq!(src(&h).last_sync_at, Some(h.now()));
    assert_eq!(h.http().requests()[0].method, "GET");
    assert_eq!(h.http().requests()[0].max_body_bytes, MAX_ICS_BYTES);
    drain(&h).await;
    assert_eq!(
        h.sql("SELECT count(*) AS n FROM reminders")[0]
            .i64("n")
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn sync_uses_exact_end_instants_across_dst() {
    for (start, end, expected) in [
        (
            "TZID=America/New_York:20260308T013000",
            ";TZID=America/New_York:20260308T033000",
            [
                ("2026-03-08T06:30:00Z", "2026-03-08T07:30:00Z"),
                ("2026-03-09T05:30:00Z", "2026-03-09T06:30:00Z"),
            ],
        ),
        (
            "TZID=America/New_York:20261101T003000",
            ";TZID=America/New_York:20261101T023000",
            [
                ("2026-11-01T04:30:00Z", "2026-11-01T07:30:00Z"),
                ("2026-11-02T05:30:00Z", "2026-11-02T08:30:00Z"),
            ],
        ),
        (
            "TZID=America/New_York:20261101T013000",
            ";TZID=Europe/London:20261101T063000",
            [
                ("2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z"),
                ("2026-11-02T06:30:00Z", "2026-11-02T07:30:00Z"),
            ],
        ),
        (
            "TZID=America/New_York:20261101T013000",
            ":20261101T063000Z",
            [
                ("2026-11-01T05:30:00Z", "2026-11-01T06:30:00Z"),
                ("2026-11-02T06:30:00Z", "2026-11-02T07:30:00Z"),
            ],
        ),
    ] {
        let h = hub();
        let feed = format!(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:dst\nDTSTART;{start}\nDTEND{end}\n\
             RRULE:FREQ=DAILY;COUNT=2\nEND:VEVENT\nEND:VCALENDAR\n"
        );
        assert_eq!(sync_with(&h, feed.as_bytes()).await.events, 1);
        let expected = expected.map(|(start, end)| (ms(start), ms(end)));
        let (occurrences, truncated) =
            list_calendar(&h.hub, expected[0].0, expected[1].1, false).unwrap();
        assert!(truncated.is_empty());
        assert_eq!(
            occurrences
                .into_iter()
                .map(|item| (item.occurrence.start_utc, item.occurrence.end_utc))
                .collect::<Vec<_>>(),
            expected,
            "DTSTART;{start} with DTEND {end}"
        );
    }
}

#[tokio::test]
async fn upgrade_refreshes_zoned_ics_without_discarding_the_last_snapshot() {
    let h = hub();
    h.set_now("2026-03-07T00:00:00Z");
    let feed = b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:dst\n\
        DTSTART;TZID=America/New_York:20260308T013000\n\
        DTEND;TZID=America/New_York:20260308T033000\nEND:VEVENT\nEND:VCALENDAR\n";
    let modified = "Fri, 06 Mar 2026 10:00:00 GMT";
    let validators = [("ETag", "\"v1\""), ("Last-Modified", modified)];
    h.http().respond(FEED, 200, &validators, feed);
    sync_due_source(&h, h.now()).await;
    drain(&h).await;
    let previous = src(&h);
    let event = load_series(&h.db, "ics:1:dst").unwrap().remove(0);
    let mut legacy = event.row;
    legacy.duration_sec = 7200;
    upsert_event(&h.db, &legacy, h.now()).unwrap();
    let reminders = h.sql("SELECT * FROM reminders");
    assert_eq!(reminders.len(), 1);
    h.sql(
        "INSERT INTO sources (id,name,url,color,etag,last_modified) VALUES
        (2,'Without zoned events','https://feeds.test/other.ics','#aabbcc','other','old')",
    );
    let mut floating = legacy.clone();
    floating.source_id = Some(2);
    floating.series_key = "ics:2:dst".into();
    floating.tzid = None;
    upsert_event(&h.db, &floating, h.now()).unwrap();
    set_meta(&h.db, "schema_version", "2").unwrap();

    h.migrate().unwrap();
    assert_eq!(get_meta(&h.db, "schema_version").as_deref(), Some("3"));
    let migrated = src(&h);
    assert_eq!(migrated.etag, None);
    assert_eq!(migrated.last_modified, None);
    assert_eq!(migrated.last_sync_at, previous.last_sync_at);
    assert_eq!(migrated.interval_min, previous.interval_min);
    assert_eq!(migrated.last_result, previous.last_result);
    let other = get_source(&h.db, 2).unwrap().unwrap();
    assert_eq!(other.etag.as_deref(), Some("other"));
    assert_eq!(other.last_modified.as_deref(), Some("old"));
    assert_eq!(h.sql("SELECT * FROM reminders"), reminders);
    assert_eq!(load_series(&h.db, "ics:1:dst").unwrap()[0].row, legacy);

    h.advance_min(30);
    h.http().respond(FEED, 200, &validators, feed);
    let summary = sync_source(&h, &migrated, h.now()).await.unwrap();
    assert_eq!(summary.enqueued, 1);
    assert!(h.http().requests()[1].headers.is_empty());
    let refreshed = load_series(&h.db, "ics:1:dst").unwrap().remove(0);
    assert_eq!(refreshed.id, event.id);
    assert_eq!(refreshed.row.duration_sec, 3600);
    drain(&h).await;
    assert_eq!(h.sql("SELECT * FROM reminders"), reminders);

    h.migrate().unwrap();
    let refreshed = src(&h);
    assert_eq!(refreshed.etag, previous.etag);
    assert_eq!(refreshed.last_modified, previous.last_modified);
    h.http().respond(FEED, 304, &[], &[]);
    assert!(
        sync_source(&h, &refreshed, h.now())
            .await
            .unwrap()
            .unchanged
    );
    assert_eq!(h.http().requests()[2].headers.len(), 2);
    assert_eq!(
        load_series(&h.db, "ics:1:dst").unwrap()[0].row.duration_sec,
        3600
    );
}

#[tokio::test]
async fn not_modified_is_unchanged() {
    let h = hub();
    let modified = "Mon, 23 Nov 2026 08:00:00 GMT";
    h.http().respond(
        FEED,
        200,
        &[("ETag", "\"v1\""), ("Last-Modified", modified)],
        SINGLE,
    );
    sync_due_source(&h, h.now()).await;
    drain(&h).await;
    let before = h.sql("SELECT * FROM events");
    h.sql(
        "UPDATE sources SET consecutive_failures = 3, alerted = 1, last_error = 'previous error'",
    );
    h.advance_min(30);
    h.http().respond(FEED, 304, &[("ETag", "\"v2\"")], &[]);
    let summary = sync_source(&h, &src(&h), h.now()).await.unwrap();
    assert_eq!(
        summary,
        SyncSummary {
            events: 0,
            enqueued: 0,
            unchanged: true
        }
    );
    assert_eq!(
        h.http().requests()[1].headers,
        [
            ("If-None-Match".into(), "\"v1\"".into()),
            ("If-Modified-Since".into(), modified.into()),
        ]
    );
    assert_eq!(h.sql("SELECT * FROM events"), before);
    assert_eq!(queue_length(&h.db).unwrap(), 0);
    let source = src(&h);
    assert_eq!(source.last_result.as_deref(), Some("unchanged"));
    assert_eq!(source.etag.as_deref(), Some("\"v2\""));
    assert_eq!(source.last_modified.as_deref(), Some(modified));
    assert_eq!(source.consecutive_failures, 0);
    assert!(!source.alerted);
    assert_eq!(source.last_error, None);
}

#[tokio::test]
async fn html_200_is_failure_and_keeps_data() {
    let h = hub();
    sync_with(&h, SINGLE).await;
    drain(&h).await;
    let before = h.sql("SELECT * FROM events");
    let reminders = h.sql("SELECT * FROM reminders");
    h.advance_min(30);
    h.http().respond(FEED, 200, &[], b"<!doctype html><html>");
    let outcome = sync_due_source(&h, h.now()).await;
    assert_eq!(
        (outcome.ok, outcome.failed, outcome.step_error),
        (0, 1, None)
    );
    assert_eq!(h.sql("SELECT * FROM events"), before);
    assert_eq!(h.sql("SELECT * FROM reminders"), reminders);
    let source = src(&h);
    assert_eq!(source.consecutive_failures, 1);
    assert_eq!(source.last_result.as_deref(), Some("ok: 1 events"));
    assert_eq!(
        source.last_error.as_deref(),
        Some("not a calendar (no VCALENDAR)")
    );
}

#[tokio::test]
async fn unreadable_event_preserves_the_last_good_snapshot() {
    let h = hub();
    h.http().respond(FEED, 200, &[("ETag", "previous")], SINGLE);
    sync_due_source(&h, h.now()).await;
    drain(&h).await;
    let events = h.sql("SELECT * FROM events");
    let reminders = h.sql("SELECT * FROM reminders");
    assert_eq!((events.len(), reminders.len()), (1, 1));

    for (fields, field) in [
        ("UID:berlin\nDTSTART:INVALID", "DTSTART"),
        ("UID:berlin", "DTSTART"),
        ("DTSTART:20261123T090000Z", "UID"),
        (
            "UID:berlin\nRECURRENCE-ID:INVALID\nSTATUS:CANCELLED",
            "RECURRENCE-ID",
        ),
    ] {
        h.advance_min(30);
        let body = format!("BEGIN:VCALENDAR\nBEGIN:VEVENT\n{fields}\nEND:VEVENT\nEND:VCALENDAR\n");
        h.http()
            .respond(FEED, 200, &[("ETag", "broken")], body.as_bytes());
        assert_eq!(sync_due_source(&h, h.now()).await.failed, 1);
        let source = src(&h);
        assert!(source.last_error.unwrap().contains(field));
        assert_eq!(source.etag.as_deref(), Some("previous"));
        assert_eq!(h.sql("SELECT * FROM events"), events);
        assert_eq!(h.sql("SELECT * FROM reminders"), reminders);
        assert_eq!(queue_length(&h.db).unwrap(), 0);
    }

    sync_with(&h, b"BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n").await;
    drain(&h).await;
    assert!(h.sql("SELECT * FROM events").is_empty());
    assert!(h.sql("SELECT * FROM reminders").is_empty());
}

#[tokio::test]
async fn oversize_without_content_length_is_failure() {
    let h = hub();
    h.http()
        .respond(FEED, 200, &[], &vec![b'x'; MAX_ICS_BYTES + 1]);
    sync_due_source(&h, h.now()).await;
    assert_eq!(
        src(&h).last_error.as_deref(),
        Some("feed too large (> 5 MB)")
    );
    assert!(h.sql("SELECT * FROM events").is_empty());
}

#[tokio::test]
async fn oversize_content_length_is_failure() {
    let h = hub();
    h.http().respond(
        FEED,
        200,
        &[("content-length", "999999999999999999999999")],
        SINGLE,
    );
    sync_due_source(&h, h.now()).await;
    assert_eq!(
        src(&h).last_error.as_deref(),
        Some("feed too large (> 5 MB)")
    );
    assert!(h.sql("SELECT * FROM events").is_empty());
}

#[tokio::test]
async fn prunes_removed_series_and_stale_overrides() {
    let h = hub();
    let single = std::str::from_utf8(SINGLE).unwrap();
    let first = format!("{OVERRIDE}{single}");
    let imported = sync_with(&h, first.as_bytes()).await;
    assert_eq!((imported.events, imported.enqueued), (3, 2));
    drain(&h).await;
    let moved_slot =
        "SELECT occ_start_utc FROM reminders WHERE key = 'ics:1:series|2026-11-30T09:00:00|15'";
    assert_eq!(
        h.sql(moved_slot)[0].i64("occ_start_utc").unwrap(),
        ms("2026-11-30T18:00:00Z")
    );
    let second = OVERRIDE.replacen(
        "BEGIN:VEVENT\nUID:series\nRECURRENCE-ID:20261130T080000Z\nDTSTART;TZID=America/New_York:20261130T130000\nEND:VEVENT\n",
        "", 1
    );
    assert_ne!(OVERRIDE, second);
    let result = sync_with(&h, second.as_bytes()).await;
    assert_eq!((result.events, result.enqueued), (1, 2));
    assert_eq!(h.sql("SELECT recurrence_id FROM events").len(), 1);
    assert_eq!(
        h.sql("SELECT recurrence_id FROM events")[0]
            .opt_text("recurrence_id")
            .unwrap(),
        None
    );
    drain(&h).await;
    assert!(
        h.sql("SELECT * FROM reminders WHERE series_key = 'ics:1:berlin'")
            .is_empty()
    );
    assert_eq!(
        h.sql(moved_slot)[0].i64("occ_start_utc").unwrap(),
        ms("2026-11-30T08:00:00Z")
    );
    let repeat = sync_with(&h, second.as_bytes()).await;
    assert_eq!((repeat.events, repeat.enqueued), (1, 0));
}

#[tokio::test]
async fn cancelled_instance_never_gets_a_reminder() {
    let h = hub();
    sync_with(&h, CANCELLED.as_bytes()).await;
    drain(&h).await;
    assert!(
        h.sql("SELECT * FROM reminders WHERE key LIKE '%2026-11-30%'")
            .is_empty()
    );
    assert!(
        !h.sql("SELECT * FROM reminders WHERE key LIKE '%2026-12-07%'")
            .is_empty()
    );
}

#[tokio::test]
async fn panicking_parse_still_advances_last_sync_at() {
    let h = hub();
    let first = sync_with(&h, SINGLE).await;
    assert_eq!(first.events, 1);
    let before = h.sql("SELECT * FROM events");
    h.advance_min(30);
    h.faults().panic_sql_matching("INSERT INTO events");
    h.http().respond(FEED, 200, &[], OVERRIDE.as_bytes());
    let outcome = sync_due_source(&h, h.now()).await;
    assert_eq!(
        (outcome.ok, outcome.failed, outcome.step_error),
        (0, 1, None)
    );
    assert_eq!(h.sql("SELECT * FROM events"), before);
    assert_eq!(src(&h).last_sync_at, Some(h.now()));
    assert_eq!(src(&h).consecutive_failures, 1);
    assert!(src(&h).last_error.unwrap().contains("panic"));
}

#[tokio::test]
async fn failed_import_rolls_back_pruning_and_insertion() {
    let h = hub();
    sync_with(&h, SINGLE).await;
    drain(&h).await;
    let before = h.sql("SELECT * FROM events");
    h.advance_min(30);
    h.faults().fail_sql_matching("INSERT INTO reconcile_queue");
    h.http()
        .respond(FEED, 200, &[("ETag", "\"unsaved\"")], OVERRIDE.as_bytes());
    let err = sync_source(&h, &src(&h), h.now()).await.unwrap_err();
    assert!(err.contains("injected SQL error"));
    assert_eq!(src(&h).last_error.as_deref(), Some(err.as_str()));
    assert_eq!(src(&h).etag, None);
    assert_eq!(h.sql("SELECT * FROM events"), before);
    assert_eq!(queue_length(&h.db).unwrap(), 0);
}

#[tokio::test]
async fn killed_sync_leaves_interrupted_trace_and_is_not_redone() {
    let h = hub();
    h.faults().kill_after_sql_matching(TRACE);
    assert!(sync_due_source(&h, h.now()).now_or_never().is_none());
    let source = src(&h);
    assert_eq!(source.last_sync_at, Some(h.now()));
    assert_eq!(source.consecutive_failures, 1);
    assert_eq!(
        source.last_error.as_deref(),
        Some("interrupted before completion (CPU or memory limit?)")
    );
    assert_eq!(pick_due_source(&h.db, h.now()).unwrap(), None);
    let replay = sync_due_source(&h, h.now()).await;
    assert_eq!((replay.ok, replay.failed, replay.step_error), (0, 0, None));
    assert!(h.http().requests().is_empty());
}

#[tokio::test]
async fn third_failure_alerts_once_even_if_killed() {
    let h = hub();
    install_bark(&h);
    for _ in 0..2 {
        h.http().respond(FEED, 503, &[], &[]);
        sync_due_source(&h, h.now()).await;
        h.advance_min(30);
    }
    h.faults().kill_after_sql_matching(TRACE);
    assert!(sync_due_source(&h, h.now()).now_or_never().is_none());
    h.http().respond(BARK, 200, &[], br#"{"code":200}"#);
    let step = send_source_alerts(&h, h.now()).await;
    assert_eq!((step.ok, step.failed, step.step_error), (1, 0, None));
    assert!(src(&h).alerted);
    assert_eq!(
        h.sql("SELECT kind, ok FROM deliveries")[0]
            .text("kind")
            .unwrap(),
        "source_alert"
    );
    assert_eq!(
        h.sql("SELECT kind, ok FROM deliveries")[0]
            .i64("ok")
            .unwrap(),
        1
    );
    let requests = h.http().requests();
    assert_eq!(requests.iter().filter(|req| req.url == BARK).count(), 1);
    let body: serde_json::Value =
        serde_json::from_slice(requests[2].body.as_deref().unwrap()).unwrap();
    assert_eq!(body["title"], "⚠️ Source sync failing");
    assert_eq!(
        body["body"],
        "Teaching: interrupted before completion (CPU or memory limit?)"
    );
    h.advance_min(30);
    h.http().respond(FEED, 503, &[], &[]);
    sync_due_source(&h, h.now()).await;
    send_source_alerts(&h, h.now()).await;
    assert_eq!(src(&h).consecutive_failures, 4);
    assert_eq!(
        h.http()
            .requests()
            .iter()
            .filter(|req| req.url == BARK)
            .count(),
        1
    );
    assert_eq!(h.sql("SELECT * FROM deliveries").len(), 1);
}

#[tokio::test]
async fn killed_alert_keeps_the_at_most_once_trace() {
    let h = hub();
    install_bark(&h);
    h.sql("UPDATE sources SET consecutive_failures = 3, last_error = 'HTTP 503'");
    h.faults()
        .kill_after_sql_matching("UPDATE sources SET alerted = 1");
    h.http().respond(BARK, 200, &[], br#"{"code":200}"#);
    assert!(send_source_alerts(&h, h.now()).now_or_never().is_none());
    assert!(src(&h).alerted);

    let replay = send_source_alerts(&h, h.now()).await;
    assert_eq!((replay.ok, replay.failed, replay.step_error), (0, 0, None));
    assert!(h.http().requests().is_empty());
    assert!(h.sql("SELECT * FROM deliveries").is_empty());
}

#[tokio::test]
async fn failed_alert_is_logged_and_not_retried() {
    let h = hub();
    h.sql("UPDATE sources SET consecutive_failures = 3, last_error = 'HTTP 503'");
    send_source_alerts(&h, h.now()).await;
    assert!(src(&h).alerted);
    let deliveries = h.sql("SELECT ok, error FROM deliveries");
    assert_eq!(deliveries[0].i64("ok").unwrap(), 0);
    assert_eq!(
        deliveries[0].text("error").unwrap(),
        "no notification channel configured"
    );
    send_source_alerts(&h, h.now()).await;
    assert_eq!(h.sql("SELECT * FROM deliveries").len(), 1);
    assert!(h.http().requests().is_empty());
}

#[tokio::test]
async fn http_url_rejected_in_production_config() {
    let mut h = hub();
    h.hub.config.allow_http_urls = false;
    h.sql("UPDATE sources SET url = 'http://feeds.test/a.ics'");
    let outcome = sync_due_source(&h, h.now()).await;
    assert_eq!(
        (outcome.ok, outcome.failed, outcome.step_error),
        (0, 1, None)
    );
    assert_eq!(
        src(&h).last_error.as_deref(),
        Some("source URL must use HTTPS")
    );
    assert!(h.http().requests().is_empty());
}

#[tokio::test]
async fn rdate_and_unknown_tzid_warnings_in_last_result() {
    let h = hub();
    let body = format!(
        "{}{}",
        include_str!("../fixtures/rdate.ics"),
        include_str!("../fixtures/unknown-tzid.ics")
    );
    sync_with(&h, body.as_bytes()).await;
    assert_eq!(
        src(&h).last_result.as_deref(),
        Some(
            r#"ok: 2 events; RDATE ignored on "Dentist"; Unknown TZID "China Standard Time" on "Standup"; using your time zone"#
        )
    );
}

#[tokio::test]
async fn invalid_utf8_retains_last_valid_feed() {
    let h = hub();
    sync_with(&h, SINGLE).await;
    h.http().respond(
        FEED,
        200,
        &[],
        b"BEGIN:VCALENDAR\nSUMMARY:\xff\nEND:VCALENDAR",
    );
    let err = sync_source(&h, &src(&h), h.now()).await.unwrap_err();
    assert_eq!(err, "feed is not valid UTF-8");
    assert_eq!(src(&h).last_error.as_deref(), Some(err.as_str()));
    assert_eq!(h.sql("SELECT * FROM events").len(), 1);
}

#[test]
fn picks_never_synced_then_oldest_due_and_skips_disabled() {
    let h = hub();
    h.sql("INSERT INTO sources (id,name,url,color,enabled) VALUES (2,'Disabled','https://example.test','#000000',0)");
    h.sql("INSERT INTO sources (id,name,url,color,interval_min) VALUES (3,'Other','https://example.test','#000000',15)");
    h.db.exec(
        "UPDATE sources SET last_sync_at=?1 WHERE id=1",
        &[Value::Integer(h.now() - 45 * 60_000)],
    )
    .unwrap();
    assert_eq!(pick_due_source(&h.db, h.now()).unwrap().unwrap().id, 3);
    h.db.exec(
        "UPDATE sources SET last_sync_at=?1 WHERE id=3",
        &[Value::Integer(h.now() - 16 * 60_000)],
    )
    .unwrap();
    assert_eq!(pick_due_source(&h.db, h.now()).unwrap().unwrap().id, 1);
    h.db.exec(
        "UPDATE sources SET last_sync_at=?1",
        &[Value::Integer(h.now())],
    )
    .unwrap();
    assert_eq!(pick_due_source(&h.db, h.now()).unwrap(), None);
}
