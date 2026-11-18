use kairos_core::{
    CoreError, Db,
    calendar::{
        events::{UpsertOutcome, add_exdate, insert_manual_event, load_series, upsert_event},
        model::{EventRow, ExpandStatus, Origin},
    },
    ports::Value,
    sources::model::{get_source, list_sources},
};
use kairos_testkit::TestHub;

fn ics_row() -> EventRow {
    EventRow {
        origin: Origin::Ics,
        source_id: Some(42),
        series_key: "ics:42:meeting".into(),
        uid: Some("meeting".into()),
        recurrence_id: None,
        external_id: None,
        title: "Meeting".into(),
        description: Some("Weekly review".into()),
        location: None,
        all_day: false,
        start_local: "2026-11-23T09:00:00".into(),
        tzid: Some("Europe/Berlin".into()),
        duration_sec: 3600,
        rrule: Some("FREQ=WEEKLY".into()),
        exdates: vec![],
        lead_minutes: None,
        valarm_minutes: vec![10],
    }
}

#[test]
fn upsert_preserves_unchanged_expansion_and_resets_changed_content() {
    let h = TestHub::new();
    let mut row = ics_row();
    let created = upsert_event(&h.db, &row, 1).unwrap();
    assert!(created.created && created.changed);
    let saved = load_series(&h.db, &row.series_key).unwrap();
    assert_eq!(saved[0].row, row);
    assert_eq!(saved[0].hash, row.content_hash());
    assert_eq!(saved[0].updated_at, 1);

    let anchor = serde_json::json!({
        "slot": row.start_local, "index": 0, "hash": row.content_hash()
    })
    .to_string();
    h.db.exec(
        "UPDATE events SET expand_anchor = ?1, expand_status = 'catching_up',
           expand_warning = 'waiting' WHERE id = ?2",
        &[Value::Text(anchor), Value::Integer(created.id)],
    )
    .unwrap();

    let unchanged = upsert_event(&h.db, &row, 2).unwrap();
    assert_eq!(
        unchanged,
        UpsertOutcome {
            id: created.id,
            created: false,
            changed: false
        }
    );
    let saved = load_series(&h.db, &row.series_key).unwrap();
    assert_eq!(saved[0].updated_at, 1);
    assert!(saved[0].expand_anchor.is_some());
    assert_eq!(saved[0].expand_status, ExpandStatus::CatchingUp);
    assert_eq!(saved[0].expand_warning.as_deref(), Some("waiting"));

    row.lead_minutes = Some(vec![]);
    let changed = upsert_event(&h.db, &row, 3).unwrap();
    assert_eq!(
        changed,
        UpsertOutcome {
            id: created.id,
            created: false,
            changed: true
        }
    );
    let saved = load_series(&h.db, &row.series_key).unwrap();
    assert_eq!(saved[0].row, row);
    assert_eq!(saved[0].hash, row.content_hash());
    assert_eq!(saved[0].updated_at, 3);
    assert_eq!(saved[0].expand_anchor, None);
    assert_eq!(saved[0].expand_status, ExpandStatus::Complete);
    assert_eq!(saved[0].expand_warning, None);
}

#[test]
fn master_and_override_have_distinct_stable_ids() {
    let h = TestHub::new();
    let master = ics_row();
    let mut moved = master.clone();
    moved.recurrence_id = Some("2026-11-30T09:00:00".into());
    moved.start_local = "2026-11-30T12:00:00".into();
    let master_id = upsert_event(&h.db, &master, 1).unwrap().id;
    let moved_id = upsert_event(&h.db, &moved, 1).unwrap().id;
    assert_ne!(master_id, moved_id);

    moved.title = "Moved meeting".into();
    assert_eq!(upsert_event(&h.db, &moved, 2).unwrap().id, moved_id);
    let saved = load_series(&h.db, &master.series_key).unwrap();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].row, master);
    assert_eq!(saved[1].row, moved);
}

#[test]
fn manual_insert_and_exdate_keep_series_identity() {
    let h = TestHub::new();
    h.db.exec(
        "INSERT INTO reminders (key, series_key, event_id, occ_start_utc, fire_at,
           next_attempt_at, status) VALUES ('old', 'manual:10', 10, 0, 0, 0, 'sent')",
        &[],
    )
    .unwrap();
    let id =
        h.db.transaction(|tx| insert_manual_event(tx, ics_row(), 1))
            .unwrap();
    assert_eq!(id, 11);
    let key = format!("manual:{id}");

    let slot = "2026-11-30T09:00:00";
    h.db.transaction(|tx| add_exdate(tx, id, slot, 2)).unwrap();
    h.db.transaction(|tx| add_exdate(tx, id, slot, 3)).unwrap();
    let saved = load_series(&h.db, &key).unwrap();
    assert_eq!(saved[0].id, id);
    assert_eq!(saved[0].row.series_key, key);
    assert_eq!(saved[0].row.origin, Origin::Manual);
    assert_eq!(saved[0].row.source_id, None);
    assert_eq!(saved[0].row.uid, None);
    assert_eq!(saved[0].row.exdates, [slot]);
    assert_eq!(saved[0].updated_at, 2);
    assert_eq!(saved[0].hash, saved[0].row.content_hash());
    assert_eq!(add_exdate(&h.db, 999, slot, 2), Err(CoreError::NotFound));
}

#[test]
fn source_store_decodes_default_and_explicit_fields() {
    let h = TestHub::new();
    h.db.exec(
        "INSERT INTO sources (id, name, url, color) VALUES
           (5, 'Classes', 'https://example.test/classes.ics', '#123456')",
        &[],
    )
    .unwrap();
    h.db.exec(
        "INSERT INTO sources (id, name, url, color, enabled, interval_min, default_lead_min,
           honor_valarm, notify, etag, last_modified, last_sync_at, last_result, last_error,
           consecutive_failures, alerted)
         VALUES (1, 'Holidays', 'https://example.test/holidays.ics', '#abcdef', 0, 15, 30,
           0, 0, 'tag', 'Yesterday', 123, 'Unchanged', 'Timeout', 3, 1)",
        &[],
    )
    .unwrap();

    let sources = list_sources(&h.db).unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].id, 1);
    assert_eq!(sources[0].name, "Holidays");
    assert_eq!(sources[0].url, "https://example.test/holidays.ics");
    assert_eq!(sources[0].color, "#abcdef");
    assert!(!sources[0].enabled);
    assert_eq!(sources[0].interval_min, 15);
    assert_eq!(sources[0].default_lead_min, Some(30));
    assert!(!sources[0].honor_valarm);
    assert!(!sources[0].notify);
    assert_eq!(sources[0].etag.as_deref(), Some("tag"));
    assert_eq!(sources[0].last_modified.as_deref(), Some("Yesterday"));
    assert_eq!(sources[0].last_sync_at, Some(123));
    assert_eq!(sources[0].last_result.as_deref(), Some("Unchanged"));
    assert_eq!(sources[0].last_error.as_deref(), Some("Timeout"));
    assert_eq!(sources[0].consecutive_failures, 3);
    assert!(sources[0].alerted);

    assert_eq!(get_source(&h.db, 5).unwrap().as_ref(), Some(&sources[1]));
    assert_eq!(get_source(&h.db, 4).unwrap(), None);
    assert!(sources[1].enabled);
    assert_eq!(sources[1].interval_min, 30);
    assert_eq!(sources[1].default_lead_min, None);
    assert!(sources[1].honor_valarm);
    assert!(sources[1].notify);
    assert_eq!(sources[1].etag, None);
    assert_eq!(sources[1].consecutive_failures, 0);
    assert!(!sources[1].alerted);
}
