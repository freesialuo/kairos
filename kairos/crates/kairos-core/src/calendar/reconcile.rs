use std::collections::HashMap;

use super::{
    events::load_series,
    model::ExpandStatus,
    plan::{PlannedReminder, plan_reminders, resolve_leads},
};
use crate::{
    CoreError, Db,
    db::settings::get_settings,
    ics::expand::{ANCHOR_LAG_MS, ExpandOpts, expand_series},
    ports::Value,
    sched::queue::enqueue_series,
    sources::model::get_source,
};

const WINDOW_MS: i64 = 60 * 86_400_000;

#[derive(Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    Done(ExpandStatus),
    Gone,
}

/// Call inside the transaction that edits this series or completes its queue item.
pub fn reconcile_series(db: &impl Db, key: &str, now: i64) -> Result<ReconcileOutcome, CoreError> {
    let rows = load_series(db, key)?;
    let Some(first) = rows.first() else {
        apply_plan(db, key, now, vec![])?;
        return Ok(ReconcileOutcome::Gone);
    };
    let settings = get_settings(db)?;
    let source = first
        .row
        .source_id
        .map(|id| get_source(db, id))
        .transpose()?
        .flatten();
    let opts = ExpandOpts {
        anchor_before_utc: Some(now.saturating_sub(ANCHOR_LAG_MS)),
        ..Default::default()
    };
    let result = expand_series(
        &rows,
        now,
        now.saturating_add(WINDOW_MS),
        &settings.timezone,
        &opts,
    );
    let mut plan = Vec::new();
    let by_id: HashMap<_, _> = rows.iter().map(|row| (row.id, &row.row)).collect();
    for occ in &result.occurrences {
        let row = by_id
            .get(&occ.event_id)
            .ok_or_else(|| CoreError::Internal("unknown occurrence event".into()))?;
        let leads = resolve_leads(row, source.as_ref(), settings.default_lead_min);
        plan.extend(plan_reminders(
            std::slice::from_ref(occ),
            &leads,
            &settings.all_day_time,
            &|_| row.tzid.as_ref().unwrap_or(&settings.timezone).clone(),
        )?);
    }
    if let Some(parent) = rows.iter().find(|row| row.row.recurrence_id.is_none())
        && (parent.expand_anchor != result.anchor
            || parent.expand_status != result.status
            || parent.expand_warning != result.warning)
    {
        let anchor = result
            .anchor
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| CoreError::Internal(e.to_string()))?;
        let status = match result.status {
            ExpandStatus::Complete => "complete",
            ExpandStatus::TooDense => "too_dense",
            ExpandStatus::CatchingUp => "catching_up",
        };
        db.exec(
            "UPDATE events SET expand_anchor=?1,expand_status=?2,expand_warning=?3 WHERE id=?4",
            &[
                anchor.map_or(Value::Null, Value::Text),
                Value::Text(status.into()),
                result.warning.map_or(Value::Null, Value::Text),
                Value::Integer(parent.id),
            ],
        )?;
    }
    apply_plan(db, key, now, plan)?;
    if result.status == ExpandStatus::CatchingUp {
        enqueue_series(db, key, "catch_up", now)?;
    }
    Ok(ReconcileOutcome::Done(result.status))
}

fn apply_plan(
    db: &impl Db,
    key: &str,
    now: i64,
    plan: Vec<PlannedReminder>,
) -> Result<(), CoreError> {
    let mut planned: HashMap<_, _> = plan
        .into_iter()
        .map(|reminder| (reminder.key.clone(), reminder))
        .collect();
    for stored in db.exec("SELECT key,fire_at,status,attempts,event_id,occ_start_utc FROM reminders WHERE series_key=?1", &[Value::Text(key.into())])? {
        let id = stored.text("key")?;
        match planned.remove(&id) {
            Some(reminder) if reminder.fire_at != stored.i64("fire_at")? => write_reminder(db, &reminder)?,
            Some(reminder) => {
                if reminder.event_id != stored.i64("event_id")? || reminder.occ_start_utc != stored.i64("occ_start_utc")? {
                    db.exec("UPDATE reminders SET event_id=?1,occ_start_utc=?2 WHERE key=?3",
                        &[Value::Integer(reminder.event_id),Value::Integer(reminder.occ_start_utc),Value::Text(id)])?;
                }
            }
            None if stored.text("status")? == "pending" => {
                if stored.i64("attempts")? > 0 {
                    db.exec("INSERT INTO deliveries(at,reminder_key,kind,channel,ok,error,summary) VALUES (?1,?2,'reminder','bark',0,'withdrawn: event changed or removed',?2)",
                        &[Value::Integer(now),Value::Text(id.clone())])?;
                }
                db.exec("DELETE FROM reminders WHERE key=?1", &[Value::Text(id)])?;
            }
            None => {}
        }
    }
    for reminder in planned.into_values() {
        write_reminder(db, &reminder)?;
    }
    Ok(())
}

fn write_reminder(db: &impl Db, r: &PlannedReminder) -> Result<(), CoreError> {
    db.exec("INSERT INTO reminders(key,series_key,event_id,occ_start_utc,lead_min,fire_at,next_attempt_at,status)
             VALUES (?1,?2,?3,?4,?5,?6,?6,'pending')
             ON CONFLICT(key) DO UPDATE SET event_id=excluded.event_id,occ_start_utc=excluded.occ_start_utc,
             fire_at=excluded.fire_at,next_attempt_at=excluded.fire_at,status='pending',attempts=0,
             late=0,last_error=NULL,sent_at=NULL",
        &[Value::Text(r.key.clone()),Value::Text(r.series_key.clone()),Value::Integer(r.event_id),
          Value::Integer(r.occ_start_utc),r.lead_min.map_or(Value::Null, |lead| Value::Integer(i64::from(lead))),Value::Integer(r.fire_at)])?;
    Ok(())
}
