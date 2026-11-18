use futures::FutureExt;
use kairos_core::{
    Db,
    calendar::{events::upsert_event, reconcile::reconcile_series},
    ics::parse::parse_ics,
    ports::Value,
    sched::dispatch::dispatch_due,
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
