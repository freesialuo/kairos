use serde::de::DeserializeOwned;

use crate::{
    CoreError, Db, DbError,
    ports::{Row, Value},
};

use super::{
    model::{EventRow, ExpandStatus, Origin, StoredEvent},
    reconcile::reconcile_series,
};

const WRITE_EVENT: &str = "
INSERT INTO events (id, origin, source_id, series_key, uid, recurrence_id, external_id,
    title, description, location, all_day, start_local, tzid, duration_sec, rrule, exdates,
    lead_minutes, valarm_minutes, hash, updated_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
ON CONFLICT (series_key, COALESCE(recurrence_id, '')) DO UPDATE SET
    origin = excluded.origin,
    source_id = excluded.source_id,
    uid = excluded.uid,
    recurrence_id = excluded.recurrence_id,
    external_id = excluded.external_id,
    title = excluded.title,
    description = excluded.description,
    location = excluded.location,
    all_day = excluded.all_day,
    start_local = excluded.start_local,
    tzid = excluded.tzid,
    duration_sec = excluded.duration_sec,
    rrule = excluded.rrule,
    exdates = excluded.exdates,
    lead_minutes = excluded.lead_minutes,
    valarm_minutes = excluded.valarm_minutes,
    hash = excluded.hash,
    updated_at = excluded.updated_at,
    expand_anchor = NULL,
    expand_status = 'complete',
    expand_warning = NULL
RETURNING id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsertOutcome {
    pub id: i64,
    pub created: bool,
    /// Whether the event content was written, including the initial insert.
    pub changed: bool,
}

/// Use the caller's transaction when updating related rows or reconciling reminders.
pub fn upsert_event(db: &impl Db, row: &EventRow, now: i64) -> Result<UpsertOutcome, CoreError> {
    let hash = row.content_hash();
    let existing = db.exec(
        "SELECT id, hash FROM events
         WHERE series_key = ?1 AND COALESCE(recurrence_id, '') = COALESCE(?2, '')",
        &[
            Value::Text(row.series_key.clone()),
            optional_text(row.recurrence_id.as_deref()),
        ],
    )?;
    if let Some(existing) = existing.first()
        && existing.text("hash")? == hash
    {
        return Ok(UpsertOutcome {
            id: existing.i64("id")?,
            created: false,
            changed: false,
        });
    }

    let id = write_event(db, row, &hash, now, None)?;
    Ok(UpsertOutcome {
        id,
        created: existing.is_empty(),
        changed: true,
    })
}

/// Allocate the ID and insert within the caller's transaction.
pub fn insert_manual_event(db: &impl Db, mut row: EventRow, now: i64) -> Result<i64, CoreError> {
    // A retained reminder still identifies its deleted manual event by this ID.
    let rows = db.exec(
        "SELECT MAX(COALESCE((SELECT MAX(id) FROM events), 0),
                    COALESCE((SELECT MAX(event_id) FROM reminders WHERE series_key GLOB 'manual:*'), 0))
         AS last_id",
        &[],
    )?;
    let id = rows
        .first()
        .ok_or_else(|| DbError("missing event id high-water mark".into()))?
        .i64("last_id")?
        .checked_add(1)
        .ok_or_else(|| DbError("event id out of range".into()))?;
    row.origin = Origin::Manual;
    row.series_key = format!("manual:{id}");
    row.source_id = None;
    row.uid = None;
    row.recurrence_id = None;
    row.external_id = None;
    write_event(db, &row, &row.content_hash(), now, Some(id))
}

pub fn load_series(db: &impl Db, series_key: &str) -> Result<Vec<StoredEvent>, CoreError> {
    db.exec(
        "SELECT * FROM events WHERE series_key = ?1 ORDER BY recurrence_id, id",
        &[Value::Text(series_key.to_owned())],
    )?
    .iter()
    .map(event_from_row)
    .collect()
}

/// The caller wraps deletion and reminder withdrawal in one transaction.
pub fn delete_series(db: &impl Db, series_key: &str, now: i64) -> Result<(), CoreError> {
    db.exec(
        "DELETE FROM events WHERE series_key = ?1",
        &[Value::Text(series_key.to_owned())],
    )?;
    reconcile_series(db, series_key, now)?;
    Ok(())
}

/// Call inside the same transaction as the subsequent reconciliation.
pub fn add_exdate(db: &impl Db, event_id: i64, slot: &str, now: i64) -> Result<(), CoreError> {
    let rows = db.exec(
        "SELECT * FROM events WHERE id = ?1",
        &[Value::Integer(event_id)],
    )?;
    let mut event = event_from_row(rows.first().ok_or(CoreError::NotFound)?)?;
    if !event.row.exdates.iter().any(|existing| existing == slot) {
        event.row.exdates.push(slot.to_owned());
        write_event(db, &event.row, &event.row.content_hash(), now, None)?;
    }
    Ok(())
}

fn write_event(
    db: &impl Db,
    row: &EventRow,
    hash: &str,
    now: i64,
    id: Option<i64>,
) -> Result<i64, CoreError> {
    let origin = match row.origin {
        Origin::Ics => "ics",
        Origin::Api => "api",
        Origin::Manual => "manual",
    };
    let rows = db.exec(
        WRITE_EVENT,
        &[
            optional_integer(id),
            Value::Text(origin.to_owned()),
            optional_integer(row.source_id),
            Value::Text(row.series_key.clone()),
            optional_text(row.uid.as_deref()),
            optional_text(row.recurrence_id.as_deref()),
            optional_text(row.external_id.as_deref()),
            Value::Text(row.title.clone()),
            optional_text(row.description.as_deref()),
            optional_text(row.location.as_deref()),
            Value::Integer(i64::from(row.all_day)),
            Value::Text(row.start_local.clone()),
            optional_text(row.tzid.as_deref()),
            Value::Integer(row.duration_sec),
            optional_text(row.rrule.as_deref()),
            Value::Text(serde_json::json!(row.exdates).to_string()),
            row.lead_minutes.as_ref().map_or(Value::Null, |leads| {
                Value::Text(serde_json::json!(leads).to_string())
            }),
            Value::Text(serde_json::json!(row.valarm_minutes).to_string()),
            Value::Text(hash.to_owned()),
            Value::Integer(now),
        ],
    )?;
    let written = rows
        .first()
        .ok_or_else(|| DbError("event write did not return an id".into()))?;
    Ok(written.i64("id")?)
}

fn event_from_row(row: &Row) -> Result<StoredEvent, CoreError> {
    let origin = match row.text("origin")?.as_str() {
        "ics" => Origin::Ics,
        "api" => Origin::Api,
        "manual" => Origin::Manual,
        value => return Err(DbError(format!("unknown event origin {value:?}")).into()),
    };
    let expand_status = match row.text("expand_status")?.as_str() {
        "complete" => ExpandStatus::Complete,
        "catching_up" => ExpandStatus::CatchingUp,
        "too_dense" => ExpandStatus::TooDense,
        value => return Err(DbError(format!("unknown expand status {value:?}")).into()),
    };
    Ok(StoredEvent {
        id: row.i64("id")?,
        row: EventRow {
            origin,
            source_id: row.opt_i64("source_id")?,
            series_key: row.text("series_key")?,
            uid: row.opt_text("uid")?,
            recurrence_id: row.opt_text("recurrence_id")?,
            external_id: row.opt_text("external_id")?,
            title: row.text("title")?,
            description: row.opt_text("description")?,
            location: row.opt_text("location")?,
            all_day: row.bool("all_day")?,
            start_local: row.text("start_local")?,
            tzid: row.opt_text("tzid")?,
            duration_sec: row.i64("duration_sec")?,
            rrule: row.opt_text("rrule")?,
            exdates: json(&row.text("exdates")?, "exdates")?,
            lead_minutes: optional_json(row, "lead_minutes")?,
            valarm_minutes: json(&row.text("valarm_minutes")?, "valarm_minutes")?,
        },
        hash: row.text("hash")?,
        updated_at: row.i64("updated_at")?,
        expand_anchor: optional_json(row, "expand_anchor")?,
        expand_status,
        expand_warning: row.opt_text("expand_warning")?,
    })
}

fn optional_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| Value::Text(value.to_owned()))
}

fn optional_integer(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

fn json<T: DeserializeOwned>(value: &str, column: &str) -> Result<T, DbError> {
    serde_json::from_str(value)
        .map_err(|error| DbError(format!("invalid JSON in {column}: {error}")))
}

fn optional_json<T: DeserializeOwned>(row: &Row, column: &str) -> Result<Option<T>, DbError> {
    row.opt_text(column)?
        .map(|value| json(&value, column))
        .transpose()
}
