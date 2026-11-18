use kairos_core::{
    Db,
    calendar::{
        events::insert_manual_event,
        model::{EventRow, Origin},
        reconcile::reconcile_series,
    },
    db::settings::{get_meta, set_meta},
    ports::Value,
    sched::{
        alarm::{Floor, WatchdogAction, run_maintenance, watchdog},
        queue::{enqueue_series, process_queue},
    },
};
use kairos_testkit::{TestHub, ms};

const DAY: i64 = 86_400_000;
fn current_maintenance(h: &TestHub) {
    set_meta(&h.db, "last_maintenance_at", &h.now().to_string()).unwrap();
}

#[tokio::test]
async fn arms_at_earliest_work_without_redundant_writes() {
    let h = TestHub::new();
    current_maintenance(&h);
    h.db.exec("INSERT INTO sources(id,name,url,color,last_sync_at) VALUES(1,'Work','https://example.org/a','red',?1)",&[Value::Integer(h.now())]).unwrap();
    h.db.exec("INSERT INTO reminders(key,series_key,event_id,occ_start_utc,fire_at,next_attempt_at,status) VALUES('k','a',1,?1,?1,?1,'pending')",&[Value::Integer(h.now()+600_000)]).unwrap();
    for _ in 0..2 {
        h.rearm(Floor::Http).await.unwrap();
    }
    assert_eq!(h.alarm_at(), Some(h.now() + 600_000));
    assert_eq!(h.set_alarm_calls(), 1);
    enqueue_series(&h.db, "a", "settings", h.now()).unwrap();
    h.rearm(Floor::Http).await.unwrap();
    assert_eq!(h.alarm_at(), Some(h.now() + 1_000));
}

#[tokio::test]
async fn error_floor_grows_resets_and_http_preserves_streak() {
    let h = TestHub::new();
    for (i, minutes) in [1, 2, 4, 8, 15, 15].into_iter().enumerate() {
        h.rearm(Floor::AlarmError).await.unwrap();
        assert_eq!(h.alarm_at(), Some(h.now() + minutes * 60_000));
        h.rearm(Floor::Http).await.unwrap();
        assert_eq!(
            get_meta(&h.db, "alarm_error_streak"),
            Some((i + 1).to_string())
        );
    }
    h.rearm(Floor::AlarmClean).await.unwrap();
    assert_eq!(h.alarm_at(), Some(h.now() + 1000));
    assert_eq!(get_meta(&h.db, "alarm_error_streak").as_deref(), Some("0"));
}

#[tokio::test]
async fn selection_error_or_panic_is_isolated_and_sets_floor() {
    for fault in [
        "SELECT key,attempts FROM reminders",
        "SELECT series_key,attempts FROM reconcile_queue",
        "SELECT * FROM sources WHERE enabled = 1",
        "SELECT id FROM sources WHERE consecutive_failures >= 3",
        "DELETE FROM auth_failures",
    ] {
        for panic in [false, true] {
            let h = TestHub::new();
            if panic {
                h.faults().panic_sql_matching(fault);
            } else {
                h.faults().fail_sql_matching(fault);
            }
            let report = h.run_alarm().await;
            assert_eq!(report.step_errors.len(), 1, "{report:?}");
            assert_eq!(get_meta(&h.db, "alarm_error_streak").as_deref(), Some("1"));
            assert!(h.alarm_at().unwrap() >= h.now() + 60_000);
            if fault == "DELETE FROM auth_failures" {
                assert_eq!(h.alarm_at(), Some(h.now() + 3_600_000));
                assert!(get_meta(&h.db, "last_maintenance_error").is_some());
            } else {
                assert_eq!(
                    get_meta(&h.db, "last_maintenance_at"),
                    Some(h.now().to_string())
                );
            }
        }
    }
}

#[tokio::test]
async fn watchdog_writes_only_for_a_missing_alarm() {
    let h = TestHub::new();
    assert_eq!(watchdog(&h).await.unwrap(), WatchdogAction::Rearmed);
    let changes = || h.sql("SELECT total_changes() AS n")[0].i64("n").unwrap();
    let n = changes();
    assert_eq!(watchdog(&h).await.unwrap(), WatchdogAction::AlarmPresent);
    assert_eq!(n, changes());
    assert_eq!(h.set_alarm_calls(), 1);
    assert_eq!(
        h.request("POST", "/internal/rearm", None, &[]).await.status,
        200
    );
}

#[test]
fn maintenance_purges_only_expired_data() {
    let h = TestHub::new();
    for (key, status, age) in [
        ("old", "sent", 91),
        ("pending", "pending", 91),
        ("recent", "sent", 89),
    ] {
        h.db.exec("INSERT INTO reminders(key,series_key,event_id,occ_start_utc,fire_at,next_attempt_at,status) VALUES(?1,'a',1,?2,?2,?2,?3)",
            &[Value::Text(key.into()),Value::Integer(h.now()-age*DAY),Value::Text(status.into())]).unwrap();
    }
    h.sql("INSERT INTO auth_failures(at) VALUES(0)");
    h.sql("INSERT INTO sessions VALUES('expired','full',0,1,0)");
    run_maintenance(&h.db, h.now()).unwrap();
    let keys: Vec<_> = h
        .sql("SELECT key FROM reminders ORDER BY key")
        .iter()
        .map(|r| r.text("key").unwrap())
        .collect();
    assert_eq!(keys, ["pending", "recent"]);
    assert!(h.sql("SELECT * FROM sessions").is_empty());
    assert!(h.sql("SELECT * FROM auth_failures").is_empty());
}

#[tokio::test]
async fn one_off_enters_the_window_after_maintenance_rolls_it() {
    let h = TestHub::new();
    let create = |wall: &str, zone: &str| {
        h.db.transaction(|db| {
            let id = insert_manual_event(
                db,
                EventRow {
                    origin: Origin::Manual,
                    source_id: None,
                    series_key: String::new(),
                    uid: None,
                    recurrence_id: None,
                    external_id: None,
                    title: "Future appointment".into(),
                    description: None,
                    location: None,
                    all_day: false,
                    start_local: wall.into(),
                    tzid: Some(zone.into()),
                    duration_sec: 0,
                    rrule: None,
                    exdates: Vec::new(),
                    lead_minutes: None,
                    valarm_minutes: Vec::new(),
                },
                h.now(),
            )?;
            reconcile_series(db, &format!("manual:{id}"), h.now())?;
            Ok(id)
        })
        .unwrap()
    };
    let entering = create("2027-01-22T00:00:00", "Pacific/Kiritimati");
    create("2027-04-12T09:00:00", "UTC");
    let run_roll = || {
        run_maintenance(&h.db, h.now()).unwrap();
        process_queue(&h, h.now())
    };
    assert!(h.sql("SELECT * FROM reminders").is_empty());
    run_roll().await;
    assert!(h.sql("SELECT * FROM reminders").is_empty());

    h.advance_min(3 * 24 * 60);
    let outcome = run_roll().await;
    assert_eq!(
        (outcome.ok, outcome.failed, outcome.step_error),
        (1, 0, None)
    );
    let reminders = h.sql("SELECT event_id, fire_at FROM reminders");
    assert_eq!(reminders.len(), 1);
    assert_eq!(reminders[0].i64("event_id").unwrap(), entering);
    assert_eq!(
        reminders[0].i64("fire_at").unwrap(),
        ms("2027-01-21T09:45:00Z")
    );
}

#[tokio::test]
async fn feed_to_bark_reminder_runs_without_browser_requests() {
    let h = TestHub::new();
    h.sql("INSERT INTO sources(id,name,url,color) VALUES(1,'Labs','https://example.test/labs','blue')");
    h.sql("INSERT INTO channels(kind,config) VALUES('bark','{\"device_key\":\"fixture\"}')");
    h.http().respond("https://example.test/labs",200,&[],b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:lab\r\nDTSTART:20261120T083000Z\r\nSUMMARY:Lab meeting\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n");
    let report = h.run_alarm().await;
    assert!(report.step_errors.is_empty(), "{report:?}");
    assert_eq!(h.alarm_at(), Some(h.now() + 1000));
    h.clock.0.set(h.alarm_at().unwrap());
    h.run_alarm().await;
    let due = kairos_testkit::ms("2026-11-20T08:15:00Z");
    assert_eq!(h.alarm_at(), Some(due));
    h.clock.0.set(due);
    h.http()
        .respond("https://api.day.app/push", 200, &[], br#"{"code":200}"#);
    h.run_alarm().await;
    h.run_alarm().await;
    assert_eq!(h.http().requests().len(), 2);
    let deliveries = h.sql("SELECT ok FROM deliveries WHERE kind='reminder'");
    assert_eq!(deliveries.len(), 1);
    assert!(deliveries[0].bool("ok").unwrap());
    assert_eq!(
        h.sql("SELECT status FROM reminders")[0]
            .text("status")
            .unwrap(),
        "sent"
    );
}
