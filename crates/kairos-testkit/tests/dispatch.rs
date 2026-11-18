use futures::FutureExt;
use kairos_core::{
    Db,
    calendar::{
        events::{load_series, upsert_event},
        reconcile::reconcile_series,
    },
    db::settings::set_meta,
    ics::parse::parse_ics,
    ports::Value,
    sched::{
        alarm::compute_next_alarm,
        dispatch::dispatch_due,
        queue::{enqueue_series, process_queue},
    },
};
use kairos_testkit::{NOW, TestHub, ms};

fn scenario(start: &str, channel: bool, seconds: i64) -> TestHub {
    let h = TestHub::new();
    let input = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:one\r\nSUMMARY:Lecture\r\nDTSTART:{start}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    );
    let mut row = parse_ics(input.as_bytes(), 1).unwrap().rows.remove(0);
    row.duration_sec = seconds;
    row.lead_minutes = Some(vec![0]);
    h.db.transaction(|db| {
        upsert_event(db, &row, h.now())?;
        reconcile_series(db, &row.series_key, h.now())?;
        Ok(())
    })
    .unwrap();
    assert_eq!(h.sql("SELECT * FROM reminders").len(), 1);
    if channel {
        h.sql("INSERT INTO channels(kind,config) VALUES ('bark','{\"device_key\":\"test\"}')");
    }
    h
}
fn respond(h: &TestHub, ok: bool) {
    h.http().respond(
        "https://api.day.app/push",
        if ok { 200 } else { 503 },
        &[],
        br#"{"code":200}"#,
    );
}
fn status(h: &TestHub) -> String {
    h.sql("SELECT status FROM reminders")[0]
        .text("status")
        .unwrap()
}

#[tokio::test]
async fn replay_after_lead_zero_send_does_not_repeat_delivery() {
    let h = scenario("20261120T080000Z", true, 3600);
    h.set_now("2026-11-20T08:00:03Z");
    respond(&h, true);
    for _ in 0..2 {
        assert_eq!(dispatch_due(&h, h.now()).await.step_error, None);
    }
    assert_eq!(status(&h), "sent");
    let requests = h.http().requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(requests[0].body.as_ref().unwrap()).unwrap()["body"],
        "08:00 · now"
    );
    assert_eq!(h.sql("SELECT * FROM deliveries").len(), 1);
}

#[tokio::test]
async fn retries_follow_backoff_and_stop_after_four_attempts() {
    let h = scenario("20261121T080000Z", true, 3600);
    h.db.exec(
        "UPDATE reminders SET fire_at=?1,next_attempt_at=?1",
        &[Value::Integer(h.now())],
    )
    .unwrap();
    for (index, wait) in [1, 5, 15, 0].into_iter().enumerate() {
        respond(&h, false);
        assert_eq!(dispatch_due(&h, h.now()).await.failed, 1);
        assert_eq!(h.http().requests().len(), index + 1);
        let row = &h.sql("SELECT * FROM reminders")[0];
        assert_eq!(row.i64("attempts").unwrap(), (index + 1) as i64);
        if wait > 0 {
            assert_eq!(row.i64("next_attempt_at").unwrap(), h.now() + wait * 60_000);
        }
        h.advance_min(wait);
    }
    assert_eq!(status(&h), "failed");
    h.advance_min(20);
    dispatch_due(&h, h.now()).await;
    assert_eq!(h.http().requests().len(), 4);
    assert_eq!(h.sql("SELECT * FROM deliveries").len(), 4);
}

#[tokio::test]
async fn deleted_events_missing_channel_and_started_occurrences_never_send() {
    for (mutation, expected) in [
        ("DELETE FROM events", None),
        ("DELETE FROM channels", Some("failed")),
        (
            "UPDATE reminders SET occ_start_utc=occ_start_utc-121000",
            Some("skipped"),
        ),
        ("UPDATE reminders SET attempts=4", Some("failed")),
    ] {
        let h = scenario("20261120T080000Z", true, 3600);
        h.sql(mutation);
        dispatch_due(&h, h.now()).await;
        assert!(h.http().requests().is_empty());
        assert_eq!(
            h.sql("SELECT status FROM reminders")
                .first()
                .map(|r| r.text("status").unwrap())
                .as_deref(),
            expected
        );
    }
}

#[tokio::test]
async fn interruption_before_send_defers_replay() {
    let h = scenario("20261120T080000Z", true, 3600);
    h.faults()
        .kill_after_sql_matching("SET attempts=attempts+1");
    assert!(dispatch_due(&h, h.now()).now_or_never().is_none());
    assert_eq!(
        h.sql("SELECT next_attempt_at FROM reminders")[0]
            .i64("next_attempt_at")
            .unwrap(),
        ms(NOW) + 60_000
    );
    dispatch_due(&h, h.now()).await;
    assert!(h.http().requests().is_empty());
    h.advance_min(1);
    respond(&h, true);
    dispatch_due(&h, h.now()).await;
    assert_eq!(h.http().requests().len(), 1);
    assert_eq!(status(&h), "sent");
}

#[tokio::test]
async fn event_edit_during_commit_cancels_the_old_send() {
    for mute in [false, true] {
        let h = scenario("20261120T080000Z", true, 3600);
        let now = h.now();
        respond(&h, true);
        h.faults().yield_next_commit();
        let mut dispatch = std::pin::pin!(dispatch_due(&h, now));
        assert!(futures::poll!(dispatch.as_mut()).is_pending());

        let key = "ics:1:one";
        let mut row = load_series(&h.db, key).unwrap().remove(0).row;
        if mute {
            row.lead_minutes = Some(vec![]);
        } else {
            row.start_local = "2026-11-20T10:00:00".into();
        }
        h.db.transaction(|db| {
            upsert_event(db, &row, now)?;
            reconcile_series(db, key, now)?;
            Ok(())
        })
        .unwrap();

        assert_eq!(dispatch.await.step_error, None);
        assert!(h.http().requests().is_empty());
        let scheduled = h.sql("SELECT status, attempts, fire_at FROM reminders");
        if mute {
            assert!(scheduled.is_empty());
        } else {
            assert_eq!(scheduled[0].text("status").unwrap(), "pending");
            assert_eq!(scheduled[0].i64("attempts").unwrap(), 0);
            assert_eq!(
                scheduled[0].i64("fire_at").unwrap(),
                ms("2026-11-20T10:00:00Z")
            );
        }
    }
}

#[tokio::test]
async fn queued_feed_change_defers_dispatch_until_recomputed() {
    let h = scenario("20261120T080000Z", true, 3600);
    let now = h.now();
    let key = "ics:1:one";
    set_meta(&h.db, "last_maintenance_at", &now.to_string()).unwrap();
    let mut row = load_series(&h.db, key).unwrap().remove(0).row;
    row.start_local = "2026-11-20T10:00:00".into();
    h.db.transaction(|db| {
        upsert_event(db, &row, now)?;
        enqueue_series(db, key, "sync", now)?;
        Ok(())
    })
    .unwrap();
    let queue_due = now + 120_000;
    h.db.exec(
        "UPDATE reconcile_queue SET not_before=?1",
        &[Value::Integer(queue_due)],
    )
    .unwrap();
    enqueue_series(&h.db, key, "roll", now).unwrap();

    assert_eq!(compute_next_alarm(&h.db, now).unwrap(), queue_due);
    assert_eq!(dispatch_due(&h, now).await.ok, 0);
    assert!(h.http().requests().is_empty());
    assert_eq!(
        h.sql("SELECT attempts FROM reminders")[0]
            .i64("attempts")
            .unwrap(),
        0
    );

    h.advance_min(2);
    assert_eq!(process_queue(&h, h.now()).await.ok, 1);
    assert_eq!(
        compute_next_alarm(&h.db, h.now()).unwrap(),
        ms("2026-11-20T10:00:00Z")
    );
    h.set_now("2026-11-20T10:00:00Z");
    respond(&h, true);
    assert_eq!(dispatch_due(&h, h.now()).await.ok, 1);
    assert_eq!(h.http().requests().len(), 1);
    assert_eq!(status(&h), "sent");
}

#[tokio::test]
async fn queued_settings_edit_during_commit_preserves_an_unchanged_reminder() {
    let h = scenario("20261120T080000Z", true, 3600);
    let now = h.now();
    let key = "ics:1:one";
    respond(&h, true);
    enqueue_series(&h.db, key, "roll", now).unwrap();
    h.faults().yield_next_commit();
    let mut dispatch = std::pin::pin!(dispatch_due(&h, now));
    assert!(futures::poll!(dispatch.as_mut()).is_pending());
    enqueue_series(&h.db, key, "settings", now).unwrap();
    assert_eq!(dispatch.await.step_error, None);
    assert!(h.http().requests().is_empty());
    let reminder = &h.sql("SELECT attempts,next_attempt_at,last_error FROM reminders")[0];
    assert_eq!(reminder.i64("attempts").unwrap(), 0);
    assert_eq!(reminder.i64("next_attempt_at").unwrap(), now);
    assert_eq!(reminder.opt_text("last_error").unwrap(), None);
    assert!(h.sql("SELECT * FROM deliveries").is_empty());

    assert_eq!(process_queue(&h, now).await.ok, 1);
    assert_eq!(dispatch_due(&h, now).await.ok, 1);
    assert_eq!(status(&h), "sent");
}

#[tokio::test]
async fn multiday_event_sends_late_on_day_two_and_skips_at_end() {
    for (now, expected) in [
        ("2026-11-22T10:00:00Z", "sent"),
        ("2026-11-24T00:00:00Z", "skipped"),
    ] {
        let h = scenario("20261121", true, 3 * 86_400);
        h.set_now(now);
        respond(&h, true);
        dispatch_due(&h, h.now()).await;
        assert_eq!(status(&h), expected);
        if expected == "sent" {
            let body: serde_json::Value =
                serde_json::from_slice(h.http().requests()[0].body.as_ref().unwrap()).unwrap();
            assert_eq!(body["title"], "(late) ⏰ Lecture");
        }
    }
}
