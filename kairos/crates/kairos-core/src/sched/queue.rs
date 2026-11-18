use super::alarm::StepOutcome;
use crate::{
    CoreError, Db, Hub, Ports,
    calendar::{
        model::ExpandStatus,
        reconcile::{ReconcileOutcome, reconcile_series},
    },
    ports::{Commit, Value},
};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub const QUEUE_BATCH: usize = 10;
pub enum SeriesFilter {
    Source(i64),
    FloatingOrAllDay,
    RecurringOnly,
    WindowRoll {
        from_local_date: String,
        before_local_date: String,
    },
    All,
}

const UPSERT: &str = "ON CONFLICT(series_key) DO UPDATE SET reason=excluded.reason,
    enqueued_at=MIN(enqueued_at,excluded.enqueued_at), attempts=0,
    not_before=MIN(not_before,excluded.not_before), last_error=NULL";

pub fn enqueue_series(db: &impl Db, key: &str, reason: &str, now: i64) -> Result<(), CoreError> {
    db.exec(
        &format!(
            "INSERT INTO reconcile_queue(series_key,reason,enqueued_at,not_before)
        VALUES (?1,?2,?3,?3) {UPSERT}"
        ),
        &[
            Value::Text(key.into()),
            Value::Text(reason.into()),
            Value::Integer(now),
        ],
    )?;
    Ok(())
}

pub fn enqueue_where(
    db: &impl Db,
    filter: SeriesFilter,
    reason: &str,
    now: i64,
) -> Result<u32, CoreError> {
    let mut params = vec![Value::Text(reason.into()), Value::Integer(now)];
    let condition = match filter {
        SeriesFilter::Source(id) => {
            params.push(Value::Integer(id));
            "source_id=?3"
        }
        SeriesFilter::FloatingOrAllDay => "tzid IS NULL OR all_day=1",
        SeriesFilter::RecurringOnly => "recurrence_id IS NULL AND rrule IS NOT NULL",
        SeriesFilter::WindowRoll {
            from_local_date,
            before_local_date,
        } => {
            params.extend([Value::Text(from_local_date), Value::Text(before_local_date)]);
            "(recurrence_id IS NULL AND rrule IS NOT NULL) OR (start_local >= ?3 AND start_local < ?4)"
        }
        SeriesFilter::All => "1",
    };
    let changed = db.exec(
        &format!(
            "INSERT INTO reconcile_queue(series_key,reason,enqueued_at,not_before)
        SELECT DISTINCT series_key,?1,?2,?2 FROM events WHERE {condition} {UPSERT} RETURNING 1"
        ),
        &params,
    )?;
    u32::try_from(changed.len()).map_err(|e| CoreError::Internal(e.to_string()))
}

pub fn queue_length(db: &impl Db) -> Result<u32, CoreError> {
    let rows = db.exec("SELECT count(*) AS n FROM reconcile_queue", &[])?;
    let n = rows
        .first()
        .ok_or_else(|| CoreError::Internal("no queue count".into()))?
        .i64("n")?;
    u32::try_from(n).map_err(|e| CoreError::Internal(e.to_string()))
}

pub async fn process_queue<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome {
    let mut result = StepOutcome::default();
    let run = async {
        let items = hub.db.exec(
            "SELECT series_key,attempts FROM reconcile_queue WHERE not_before<=?1
            ORDER BY not_before,enqueued_at,series_key LIMIT ?2",
            &[Value::Integer(now), Value::Integer(QUEUE_BATCH as i64)],
        )?;
        for item in items {
            let key = item.text("series_key")?;
            let attempts = item.i64("attempts")?;
            if attempts >= 3 {
                // A kill during the final attempt left the committed trace but no failure handler.
                hub.db.transaction(|db| discard(db, &key, "interrupted"))?;
                result.failed += 1;
                continue;
            }
            let attempt = attempts + 1;
            let backoff = now.saturating_add((1_i64 << attempt.clamp(0, 3)) * 60_000);
            let trace = hub.db.exec("UPDATE reconcile_queue SET attempts=attempts+1, not_before=?2,last_error='interrupted'
                WHERE series_key=?1 AND not_before<=?3 AND attempts=?4 RETURNING 1",
                &[Value::Text(key.clone()),Value::Integer(backoff),Value::Integer(now),Value::Integer(attempts)])?;
            if trace.is_empty() {
                continue;
            }
            hub.commit.commit_point().await?;
            // Commit the computed reminders and removal/replacement of their queue item together.
            let work = catch_unwind(AssertUnwindSafe(|| {
                hub.db.transaction(|db| {
                    let outcome = reconcile_series(db, &key, now)?;
                    if outcome != ReconcileOutcome::Done(ExpandStatus::CatchingUp) {
                        db.exec(
                            "DELETE FROM reconcile_queue WHERE series_key=?1",
                            &[Value::Text(key.clone())],
                        )?;
                    }
                    Ok(())
                })
            }));
            let failure = match work {
                Ok(Ok(())) => {
                    result.ok += 1;
                    continue;
                }
                Ok(Err(e)) => e.to_string(),
                Err(_) => "panic during reminder recomputation".into(),
            };
            result.failed += 1;
            if attempt >= 3 {
                hub.db.transaction(|db| discard(db, &key, &failure))?;
            } else {
                hub.db.exec(
                    "UPDATE reconcile_queue SET last_error=?2 WHERE series_key=?1",
                    &[Value::Text(key), Value::Text(failure)],
                )?;
            }
        }
        Ok::<(), CoreError>(())
    };
    use futures::FutureExt;
    result.step_error = match AssertUnwindSafe(run).catch_unwind().await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("panic while processing the reconciliation queue".into()),
    };
    result
}

fn discard(db: &impl Db, key: &str, message: &str) -> Result<(), CoreError> {
    db.exec(
        "UPDATE events SET expand_warning=?2 WHERE series_key=?1 AND recurrence_id IS NULL",
        &[
            Value::Text(key.into()),
            Value::Text(format!("Reminder recomputation failed: {message}")),
        ],
    )?;
    db.exec(
        "DELETE FROM reconcile_queue WHERE series_key=?1",
        &[Value::Text(key.into())],
    )?;
    Ok(())
}
