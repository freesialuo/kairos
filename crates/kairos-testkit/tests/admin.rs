use kairos_core::{
    Db,
    calendar::events::upsert_event,
    db::settings::{get_settings, put_settings},
    ics::parse::parse_ics,
    ports::Value,
    sched::queue::process_queue,
};
use kairos_testkit::{TestHub, ms};
use serde_json::{Value as Json, json};

async fn send(
    h: &TestHub,
    c: &str,
    m: &str,
    path: &str,
    body: Option<Json>,
) -> kairos_testkit::TestResponse {
    h.request(m, &format!("/api/admin/{path}"), body, &[c])
        .await
}

#[tokio::test]
async fn management_requires_a_full_session_and_export_excludes_credentials() {
    let h = TestHub::new();
    for route in ["settings", "log", "sources", "export.json", "export.ics"] {
        assert_eq!(
            h.request("GET", &format!("/api/admin/{route}"), None, &[])
                .await
                .status,
            401
        );
    }
    let c = h.admin_session().await;
    let channel =
        json!({"server":"https://bark.test","device_key":"private-device-key","enabled":true});
    assert_eq!(
        send(&h, &c, "PUT", "channel", Some(channel)).await.status,
        200
    );
    h.http()
        .respond("https://bark.test/push", 200, &[], br#"{"code":200}"#);
    assert_eq!(
        send(&h, &c, "POST", "channel/test", Some(json!({})))
            .await
            .status,
        200
    );
    let token = send(&h, &c, "POST", "tokens", Some(json!({"name":"script"})))
        .await
        .json();
    let secret = token["token"].as_str().unwrap();
    assert!(secret.starts_with("kr_"));
    assert!(
        !send(&h, &c, "GET", "tokens", None)
            .await
            .json()
            .to_string()
            .contains(secret)
    );
    let export = send(&h, &c, "GET", "export.json", None).await.json();
    let serialized = export.to_string();
    assert!(!serialized.contains("private-device-key"));
    assert!(!serialized.contains(secret));
    for k in ["passkeys", "sessions", "tokens"] {
        assert!(export.get(k).is_none());
    }
    let log = send(&h, &c, "GET", "log", None).await.json();
    assert_eq!(log["deliveries"][0]["kind"], "test");
    assert!(log["background"].is_object());
    let keys = send(&h, &c, "GET", "passkeys", None).await.json();
    let path = format!("passkeys/{}", keys[0]["id"].as_str().unwrap());
    assert_eq!(
        send(&h, &c, "PATCH", &path, Some(json!({"name":"Laptop"})))
            .await
            .status,
        204
    );
    assert_eq!(
        send(&h, &c, "DELETE", &path, None).await.json()["error"],
        "confirm_required"
    );
    assert_eq!(
        send(&h, &c, "DELETE", &format!("{path}?confirm=1"), None)
            .await
            .status,
        204
    );
}
#[tokio::test]
async fn manual_series_skip_calendar_and_ics_round_trip() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let input = json!({"title":"Standup, <b>test</b>","start":"2026-11-23T09:00:00","timezone":"UTC",
        "repeat":{"freq":{"weekly":{"days":["MO"]}},"interval":1,"end":{"count":3}},"reminders":{"custom":[10]}});
    let created = send(&h, &c, "POST", "events", Some(input)).await;
    assert_eq!(created.status, 200, "{:?}", created.json());
    let id = created.json()["id"].as_i64().unwrap();
    assert_eq!(
        send(
            &h,
            &c,
            "POST",
            &format!("events/{id}/skip"),
            Some(json!({"slot":"2026-11-30T09:00:00"}))
        )
        .await
        .status,
        204
    );
    let calendar = send(
        &h,
        &c,
        "GET",
        "calendar?from=2026-11-20T00:00:00Z&to=2026-12-10T00:00:00Z",
        None,
    )
    .await
    .json();
    let occ = calendar["occurrences"].as_array().unwrap();
    assert_eq!(occ.len(), 2);
    assert_eq!(occ[0]["editable"], true);
    assert_eq!(occ[0]["deletable"], true);
    assert_eq!(occ[0]["reminders"][0]["status"], "pending");
    let response = send(&h, &c, "GET", "export.ics", None).await;
    let parsed = parse_ics(&response.body, 1).unwrap();
    assert_eq!(parsed.rows[0].title, "Standup, <b>test</b>");
    assert_eq!(parsed.rows[0].exdates, ["2026-11-30T09:00:00"]);
    assert_eq!(
        send(&h, &c, "GET", "events", None).await.json()[0]["end_local"],
        "2026-11-23T09:00:00"
    );
}

#[tokio::test]
async fn manual_event_remains_saved_when_alarm_rearm_fails() {
    let h = TestHub::new();
    let session = h.admin_session().await;
    h.faults().fail_sql_matching("SELECT MIN(at) AS earliest");

    let response = send(
        &h,
        &session,
        "POST",
        "events",
        Some(json!({"title":"Checkup","start":"2026-11-21T09:00:00","timezone":"UTC"})),
    )
    .await;
    assert_eq!(response.status, 200);
    let event = send(&h, &session, "GET", "events", None).await.json();
    assert_eq!(event[0]["id"], response.json()["id"]);
    assert_eq!(event[0]["event"]["title"], "Checkup");
    assert_eq!(
        h.sql("SELECT key FROM reminders WHERE status='pending'")
            .len(),
        1
    );

    let log = send(&h, &session, "GET", "log", None).await.json();
    let diagnostic = log["background"]["last_alarm_error"].as_str().unwrap();
    assert!(diagnostic.contains("injected SQL error"), "{diagnostic}");
}

#[tokio::test]
async fn calendar_returns_ongoing_all_day_events_and_their_reminders() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let retreat = send(
        &h,
        &c,
        "POST",
        "events",
        Some(json!({
            "title": "Retreat", "start": "2026-11-20", "end": "2026-11-26",
            "location": "Cabin", "all_day": true, "reminders": "default"
        })),
    )
    .await;
    assert_eq!(retreat.status, 200, "{:?}", retreat.json());
    let id = retreat.json()["id"].as_i64().unwrap();
    for input in [
        json!({
            "title": "Ended at the boundary", "start": "2026-11-20", "end": "2026-11-22", "all_day": true,
            "reminders": "none"
        }),
        json!({
            "title": "Timed event started earlier", "start": "2026-11-20T21:00:00",
            "end": "2026-11-27T21:00:00", "reminders": "none"
        }),
        json!({
            "title": "Starts at the upper boundary", "start": "2026-11-30", "all_day": true,
            "reminders": "none"
        }),
    ] {
        let event = send(&h, &c, "POST", "events", Some(input)).await;
        assert_eq!(event.status, 200, "{:?}", event.json());
    }
    h.sql("UPDATE reminders SET status='sent' WHERE status='pending'");
    h.set_now("2026-11-23T08:00:00Z");

    let calendar = send(
        &h,
        &c,
        "GET",
        "calendar?from=2026-11-22T00:00:00Z&to=2026-11-30T00:00:00Z",
        None,
    )
    .await;
    assert_eq!(calendar.status, 200);
    let body = calendar.json();
    assert_eq!(body["truncated"], json!([]));
    let items = body["occurrences"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    let retreat = &items[0];
    assert_eq!(retreat["event_id"], id);
    assert_eq!(retreat["title"], "Retreat");
    assert_eq!(retreat["location"], "Cabin");
    assert_eq!(retreat["all_day"], true);
    assert_eq!(retreat["editable"], true);
    assert_eq!(retreat["slot"], "single");
    assert_eq!(retreat["start_local"], "2026-11-20");
    assert_eq!(retreat["end_local"], "2026-11-26");
    assert_eq!(
        retreat["reminders"],
        json!([{
            "key": format!("{}|allday", retreat["occ_key"].as_str().unwrap()),
            "status": "sent", "fire_at": ms("2026-11-20T09:00:00Z")
        }])
    );
}

#[tokio::test]
async fn calendar_includes_recurring_all_day_parents_and_overrides_across_dst() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let mut settings = get_settings(&h.db).unwrap();
    settings.timezone = "America/New_York".into();
    put_settings(&h.db, &settings).unwrap();
    let source = send(
        &h,
        &c,
        "POST",
        "sources",
        Some(json!({"name": "Classes", "url": "https://feeds.test/work"})),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let feed = b"BEGIN:VCALENDAR\r\n\
        BEGIN:VEVENT\r\nUID:work\r\nSUMMARY:Course\r\n\
        DTSTART;VALUE=DATE:20261031\r\nDTEND;VALUE=DATE:20261103\r\n\
        RRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE;VALUE=DATE:20261114\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:work\r\nRECURRENCE-ID;VALUE=DATE:20261107\r\n\
        SUMMARY:Moved course\r\nDTSTART;VALUE=DATE:20261030\r\n\
        DTEND;VALUE=DATE:20261104\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let rows = parse_ics(feed, source).unwrap().rows;
    let ids = rows
        .iter()
        .map(|row| upsert_event(&h.db, row, h.now()).unwrap().id)
        .collect::<Vec<_>>();

    let calendar = send(
        &h,
        &c,
        "GET",
        "calendar?from=2026-11-03T04:30:00Z&to=2026-11-15T00:00:00Z",
        None,
    )
    .await;
    assert_eq!(calendar.status, 200);
    let body = calendar.json();
    let items = body["occurrences"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let moved = &items[0];
    assert_eq!(moved["event_id"], ids[1]);
    assert_eq!(moved["slot"], "2026-11-07");
    assert_eq!(moved["title"], "Moved course");
    assert_eq!(moved["source"], "Classes");
    assert_eq!(moved["start_utc"], ms("2026-10-30T04:00:00Z"));
    assert_eq!(moved["end_utc"], ms("2026-11-04T05:00:00Z"));
    let parent = &items[1];
    assert_eq!(parent["event_id"], ids[0]);
    assert_eq!(parent["slot"], "2026-10-31");
    assert_eq!(parent["title"], "Course");
    assert_eq!(parent["start_utc"], ms("2026-10-31T04:00:00Z"));
    assert_eq!(parent["end_utc"], ms("2026-11-03T05:00:00Z"));
}

#[tokio::test]
async fn source_switch_withdraws_and_calendar_zone_changes_across_dst() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let mut s = get_settings(&h.db).unwrap();
    s.timezone = "Asia/Shanghai".into();
    put_settings(&h.db, &s).unwrap();
    let source = send(
        &h,
        &c,
        "POST",
        "sources",
        Some(json!({"name":"Classes","url":"https://feeds.test/work"})),
    )
    .await
    .json()["id"]
        .as_i64()
        .unwrap();
    let feed=b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:work\r\nSUMMARY:Class\r\nDTSTART;TZID=America/New_York:20261019T090000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let row = parse_ics(feed, source).unwrap().rows.remove(0);
    upsert_event(&h.db, &row, h.now()).unwrap();
    let url = "calendar?from=2026-10-19T00:00:00Z&to=2026-11-15T23:59:59Z";
    let list = send(&h, &c, "GET", url, None).await.json();
    let times: Vec<_> = list["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| &v["start_local"].as_str().unwrap()[11..16])
        .collect();
    assert_eq!(times, ["21:00", "21:00", "22:00", "22:00"]);
    let address = format!("sources/{source}");
    send(&h, &c, "PATCH", &address, Some(json!({"notify":true}))).await;
    process_queue(&h, h.now()).await;
    assert!(!h.sql("SELECT key FROM reminders").is_empty());
    send(&h, &c, "PATCH", &address, Some(json!({"notify":false}))).await;
    process_queue(&h, h.now()).await;
    assert!(h.sql("SELECT key FROM reminders").is_empty());
    send(&h, &c, "PATCH", &address, Some(json!({"enabled":false}))).await;
    assert_eq!(
        send(&h, &c, "GET", url, None).await.json()["occurrences"],
        json!([])
    );
    // Changing URL discards the old conditional request validators.
    h.db.exec(
        "UPDATE sources SET etag='old',last_modified='yesterday' WHERE id=?1",
        &[Value::Integer(source)],
    )
    .unwrap();
    send(
        &h,
        &c,
        "PATCH",
        &address,
        Some(json!({"url":"https://example.org/new"})),
    )
    .await;
    assert!(
        h.sql("SELECT * FROM sources")[0]
            .opt_text("etag")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn legacy_event_link_resolves_an_out_of_window_start_in_display_timezone() {
    let h = TestHub::new();
    let c = h.admin_session().await;
    let event = send(
        &h,
        &c,
        "POST",
        "events",
        Some(json!({
            "title": "Tokyo appointment", "start":"2027-06-01T00:30:00",
            "timezone":"Asia/Tokyo", "reminders":"none"
        })),
    )
    .await;
    assert_eq!(event.status, 200, "{:?}", event.json());
    let href = format!("events/{}/calendar-date", event.json()["id"]);
    let date = send(&h, &c, "GET", &href, None).await;
    assert_eq!(date.status, 200);
    assert_eq!(date.json()["date"], "2027-05-31");
    assert_eq!(
        send(&h, &c, "GET", "events/9999/calendar-date", None)
            .await
            .status,
        404
    );
}
