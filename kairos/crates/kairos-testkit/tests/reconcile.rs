use futures::FutureExt;
use kairos_core::{
    Db,
    calendar::{
        events::{delete_series, load_series, upsert_event},
        model::{EventRow, ExpandStatus, Origin},
        plan::{Leads, resolve_leads},
        reconcile::{ReconcileOutcome, reconcile_series},
    },
    ports::Value,
    sched::queue::{SeriesFilter, enqueue_series, enqueue_where, process_queue, queue_length},
    sources::model::get_source,
};
use kairos_testkit::{TestHub, ms};

fn event(key: &str) -> EventRow {
    EventRow {
        origin: Origin::Api,
        series_key: key.into(),
        source_id: None,
        uid: None,
        recurrence_id: None,
        external_id: None,
        title: "Lecture".into(),
        description: None,
        location: None,
        all_day: false,
        start_local: "2026-11-21T10:00:00".into(),
        initial_start_utc: None,
        tzid: Some("UTC".into()),
        duration_sec: 3600,
        rrule: None,
        exdates: vec![],
        valarm_minutes: vec![],
        lead_minutes: None,
    }
}
fn reconcile(h: &TestHub, key: &str) -> ReconcileOutcome {
    h.db.transaction(|db| reconcile_series(db, key, h.now()))
        .unwrap()
}
fn seed_source(h: &TestHub) {
    h.sql("INSERT INTO sources(id,name,url,color,default_lead_min) VALUES(1,'classes','https://example.org/cal','#123456',40)");
}

#[test]
fn lead_precedence_honors_source_and_explicit_mutes() {
    let h = TestHub::new();
    seed_source(&h);
    let mut source = get_source(&h.db, 1).unwrap().unwrap();
    let mut row = event("ics:1:a");
    let leads = |row: &EventRow, source: &_| resolve_leads(row, Some(source), 15);
    assert_eq!(resolve_leads(&row, None, 15), Leads::Minutes(vec![15]));
    assert_eq!(leads(&row, &source), Leads::Minutes(vec![40]));
    row.valarm_minutes = vec![60, 10, 60];
    assert_eq!(leads(&row, &source), Leads::Minutes(vec![10, 60]));
    source.honor_valarm = false;
    assert_eq!(leads(&row, &source), Leads::Minutes(vec![40]));
    row.lead_minutes = Some(vec![20, 5, 20]);
    assert_eq!(leads(&row, &source), Leads::Minutes(vec![5, 20]));
    source.enabled = false;
    assert_eq!(leads(&row, &source), Leads::Muted);
    source.enabled = true;
    source.notify = false;
    assert_eq!(leads(&row, &source), Leads::Muted);
    row.lead_minutes = Some(vec![]);
    assert_eq!(resolve_leads(&row, None, 15), Leads::Muted);
}

#[test]
fn changed_time_reopens_reminder_and_unchanged_reconcile_keeps_retry() {
    let h = TestHub::new();
    let key = "api:lecture";
    let mut row = event(key);
    let initial = upsert_event(&h.db, &row, h.now()).unwrap();
    assert!(initial.created && initial.changed);
    assert_eq!(
        reconcile(&h, key),
        ReconcileOutcome::Done(ExpandStatus::Complete)
    );
    let reminder = &h.sql("SELECT * FROM reminders")[0];
    assert_eq!(reminder.text("key").unwrap(), "api:lecture|single|15");
    assert_eq!(reminder.i64("fire_at").unwrap(), ms("2026-11-21T09:45:00Z"));
    h.sql("UPDATE reminders SET attempts=2,next_attempt_at=123,last_error='transient'");
    reconcile(&h, key);
    assert_eq!(
        h.sql("SELECT next_attempt_at FROM reminders")[0]
            .i64("next_attempt_at")
            .unwrap(),
        123
    );
    h.sql("UPDATE reminders SET status='sent',sent_at=123");
    row.start_local = "2026-11-21T12:00:00".into();
    upsert_event(&h.db, &row, h.now()).unwrap();
    reconcile(&h, key);
    let reminder = &h.sql("SELECT * FROM reminders")[0];
    assert_eq!(reminder.text("status").unwrap(), "pending");
    assert_eq!(reminder.i64("attempts").unwrap(), 0);
    assert_eq!(
        reminder.i64("next_attempt_at").unwrap(),
        ms("2026-11-21T11:45:00Z")
    );
    assert_eq!(reminder.opt_i64("sent_at").unwrap(), None);
}

#[test]
fn withdrawal_logs_attempted_pending_reminders_once_and_preserves_history() {
    let h = TestHub::new();
    let key = "api:lecture";
    let mut row = event(key);
    row.lead_minutes = Some(vec![10, 20, 30]);
    upsert_event(&h.db, &row, h.now()).unwrap();
    reconcile(&h, key);
    h.sql("UPDATE reminders SET attempts=1 WHERE lead_min=10");
    h.sql("UPDATE reminders SET status='sent',attempts=1 WHERE lead_min=20");
    h.db.transaction(|db| delete_series(db, key, h.now()))
        .unwrap();
    assert_eq!(reconcile(&h, key), ReconcileOutcome::Gone);
    let remaining = h.sql("SELECT status FROM reminders");
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].text("status").unwrap(), "sent");
    let deliveries = h.sql("SELECT * FROM deliveries");
    assert_eq!(deliveries.len(), 1);
    assert_eq!(
        deliveries[0].text("reminder_key").unwrap(),
        "api:lecture|single|10"
    );
    assert_eq!(
        deliveries[0].text("error").unwrap(),
        "withdrawn: event changed or removed"
    );
}

#[test]
fn all_day_first_date_at_nine_and_mute_withdraws_it() {
    let h = TestHub::new();
    seed_source(&h);
    let mut row = event("ics:1:holiday");
    row.origin = Origin::Ics;
    row.source_id = Some(1);
    row.all_day = true;
    row.tzid = Some("Europe/Berlin".into());
    row.start_local = "2026-12-25".into();
    row.duration_sec = 3 * 86400;
    row.lead_minutes = Some(vec![15, 60]);
    upsert_event(&h.db, &row, h.now()).unwrap();
    reconcile(&h, &row.series_key);
    let reminders = h.sql("SELECT * FROM reminders");
    assert_eq!(reminders.len(), 1);
    assert_eq!(
        reminders[0].text("key").unwrap(),
        "ics:1:holiday|single|allday"
    );
    assert_eq!(reminders[0].opt_i64("lead_min").unwrap(), None);
    assert_eq!(
        reminders[0].i64("fire_at").unwrap(),
        ms("2026-12-25T08:00:00Z")
    );
    h.sql("UPDATE sources SET notify=0");
    reconcile(&h, &row.series_key);
    assert!(h.sql("SELECT * FROM reminders").is_empty());
}

#[tokio::test]
async fn queue_drains_ten_per_call_and_enqueues_each_series_once() {
    let h = TestHub::new();
    for n in 0..25 {
        upsert_event(&h.db, &event(&format!("api:{n}")), h.now()).unwrap();
    }
    for _ in 0..2 {
        assert_eq!(
            enqueue_where(&h.db, SeriesFilter::All, "settings", h.now()).unwrap(),
            25
        );
    }
    for count in [15, 5, 0] {
        let step = process_queue(&h, h.now()).await;
        assert_eq!(step.step_error, None);
        assert_eq!(step.failed, 0);
        assert_eq!(queue_length(&h.db).unwrap(), count);
    }
    assert_eq!(h.sql("SELECT key FROM reminders").len(), 25);
}

#[tokio::test]
async fn queue_stops_before_reminder_computation_when_commit_fails() {
    let h = TestHub::new();
    let key = "api:commit-failure";
    upsert_event(&h.db, &event(key), h.now()).unwrap();
    enqueue_series(&h.db, key, "new", h.now()).unwrap();
    h.faults().fail_next_commit();

    let failed = process_queue(&h, h.now()).await;
    assert_eq!(
        failed.step_error.as_deref(),
        Some("database: injected commit failure")
    );
    assert_eq!(failed.ok, 0);
    assert_eq!(queue_length(&h.db).unwrap(), 1);
    assert!(h.sql("SELECT key FROM reminders").is_empty());

    h.advance_min(2);
    let retried = process_queue(&h, h.now()).await;
    assert_eq!(retried.step_error, None);
    assert_eq!(retried.ok, 1);
    assert_eq!(queue_length(&h.db).unwrap(), 0);
    assert_eq!(h.sql("SELECT key FROM reminders").len(), 1);
}

#[tokio::test]
async fn queue_trace_survives_kill_and_failure_warning_survives_drop() {
    let h = TestHub::new();
    let key = "api:stuck";
    upsert_event(&h.db, &event(key), h.now()).unwrap();
    enqueue_series(&h.db, key, "new", h.now()).unwrap();
    h.faults()
        .kill_after_sql_matching("SET attempts=attempts+1");
    assert!(process_queue(&h, h.now()).now_or_never().is_none());
    let trace = &h.sql("SELECT * FROM reconcile_queue")[0];
    assert_eq!(trace.i64("attempts").unwrap(), 1);
    assert_eq!(trace.i64("not_before").unwrap(), h.now() + 120_000);
    assert_eq!(trace.text("last_error").unwrap(), "interrupted");
    assert_eq!(process_queue(&h, h.now()).await.ok, 0);
    // This SELECT occurs inside each reconciliation transaction.
    h.faults()
        .panic_sql_matching("SELECT key,value FROM settings WHERE key IN");
    for minutes in [2, 4] {
        h.advance_min(minutes);
        let step = process_queue(&h, h.now()).await;
        assert_eq!((step.failed, step.step_error), (1, None));
    }
    assert_eq!(queue_length(&h.db).unwrap(), 0);
    assert!(
        load_series(&h.db, key).unwrap()[0]
            .expand_warning
            .as_deref()
            .unwrap()
            .starts_with("Reminder recomputation failed:")
    );
    assert!(h.sql("SELECT * FROM reminders").is_empty());
}

#[tokio::test]
async fn catching_up_requeues_without_consuming_failure_attempts() {
    let h = TestHub::new();
    let key = "api:old";
    let mut row = event(key);
    row.start_local = "2020-01-01T08:00:00".into();
    row.rrule = Some("FREQ=HOURLY".into());
    upsert_event(&h.db, &row, h.now()).unwrap();
    enqueue_series(&h.db, key, "roll", h.now()).unwrap();
    let step = process_queue(&h, h.now()).await;
    assert_eq!(step.ok, 1);
    let trace = &h.sql("SELECT * FROM reconcile_queue")[0];
    assert_eq!(trace.text("reason").unwrap(), "catch_up");
    assert_eq!(trace.i64("attempts").unwrap(), 0);
    for _ in 0..50 {
        if queue_length(&h.db).unwrap() == 0 {
            break;
        }
        let outcome = process_queue(&h, h.now()).await;
        assert_eq!((outcome.ok, outcome.step_error), (1, None));
    }
    assert_eq!(queue_length(&h.db).unwrap(), 0);
    assert!(
        !h.db
            .exec(
                "SELECT key FROM reminders WHERE series_key=?1 LIMIT 1",
                &[Value::Text(key.into())]
            )
            .unwrap()
            .is_empty()
    );
}
