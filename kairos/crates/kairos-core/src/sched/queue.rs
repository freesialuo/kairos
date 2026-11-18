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

// Daily rolls add horizon coverage without invalidating the existing schedule.
// Catch-up passes retain any invalidation that was already queued.
pub(super) const REMINDER_SCHEDULE_READY: &str = "NOT EXISTS (
    SELECT 1 FROM reconcile_queue q WHERE q.series_key=reminders.series_key
    AND q.reason NOT IN ('roll','catch_up'))";

const UPSERT: &str = "ON CONFLICT(series_key) DO UPDATE SET reason=CASE
    WHEN excluded.reason='catch_up' AND reconcile_queue.reason<>'roll' THEN reconcile_queue.reason
    ELSE excluded.reason END,
    enqueued_at=MIN(enqueued_at,excluded.enqueued_at), attempts=0,
    not_before=MIN(not_before,excluded.not_before), last_error=NULL WHERE excluded.reason<>'roll'";

const CURRENT_ATTEMPT: &str = "series_key=?1 AND attempts=?2 AND not_before=?3";

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
            "SELECT series_key,attempts,not_before FROM reconcile_queue WHERE not_before<=?1
            ORDER BY not_before,enqueued_at,series_key LIMIT ?2",
            &[Value::Integer(now), Value::Integer(QUEUE_BATCH as i64)],
        )?;
        for item in items {
            let key = item.text("series_key")?;
            let attempts = item.i64("attempts")?;
            let before = [
                Value::Text(key.clone()),
                Value::Integer(attempts),
                Value::Integer(item.i64("not_before")?),
            ];
            if attempts >= 3 {
                // A kill during the final attempt left the committed trace but no failure handler.
                let discarded = hub
                    .db
                    .transaction(|db| discard(db, &key, "interrupted", now, &before))?;
                result.failed += u32::from(discarded);
                continue;
            }
            let attempt = attempts + 1;
            let backoff = now.saturating_add((1_i64 << attempt.clamp(0, 3)) * 60_000);
            let mut claim = before.to_vec();
            claim.push(Value::Integer(backoff));
            let trace = hub.db.exec(&format!("UPDATE reconcile_queue SET attempts=attempts+1, not_before=?4,last_error='interrupted'
                WHERE {CURRENT_ATTEMPT} RETURNING 1"), &claim)?;
            if trace.is_empty() {
                continue;
            }
            hub.commit.commit_point().await?;
            let current = [
                Value::Text(key.clone()),
                Value::Integer(attempt),
                Value::Integer(backoff),
            ];
            // Commit the computed reminders and removal/replacement of their queue item together.
            let work = catch_unwind(AssertUnwindSafe(|| {
                hub.db.transaction(|db| {
                    // A newer edit can reset this job while the trace is committing.
                    let owned = db.exec(
                        &format!("SELECT 1 FROM reconcile_queue WHERE {CURRENT_ATTEMPT}"),
                        &current,
                    )?;
                    if owned.is_empty() {
                        return Ok(false);
                    }
                    let outcome = reconcile_series(db, &key, now)?;
                    if outcome != ReconcileOutcome::Done(ExpandStatus::CatchingUp) {
                        db.exec(
                            "DELETE FROM reconcile_queue WHERE series_key=?1",
                            &[Value::Text(key.clone())],
                        )?;
                    }
                    Ok(true)
                })
            }));
            let failure = match work {
                Ok(Ok(reconciled)) => {
                    result.ok += u32::from(reconciled);
                    continue;
                }
                Ok(Err(e)) => e.to_string(),
                Err(_) => "panic during reminder recomputation".into(),
            };
            result.failed += 1;
            if attempt >= 3 {
                hub.db
                    .transaction(|db| discard(db, &key, &failure, now, &current))?;
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

fn discard(
    db: &impl Db,
    key: &str,
    message: &str,
    now: i64,
    attempt: &[Value; 3],
) -> Result<bool, CoreError> {
    let discarded = db.exec(
        &format!("DELETE FROM reconcile_queue WHERE {CURRENT_ATTEMPT} RETURNING reason"),
        attempt,
    )?;
    let Some(discarded) = discarded.first() else {
        return Ok(false);
    };
    db.exec(
        "UPDATE events SET expand_warning=?2 WHERE series_key=?1",
        &[
            Value::Text(key.into()),
            Value::Text(format!("Reminder recomputation failed: {message}")),
        ],
    )?;
    if !matches!(discarded.text("reason")?.as_str(), "roll" | "catch_up") {
        // A failed invalidation cannot make the old schedule eligible again.
        db.exec(
            "INSERT INTO deliveries(at,reminder_key,kind,channel,ok,error,summary)
             SELECT ?2,key,'reminder','bark',0,'withdrawn: reminder recomputation failed',key
             FROM reminders WHERE series_key=?1 AND status='pending' AND attempts>0",
            &[Value::Text(key.into()), Value::Integer(now)],
        )?;
        db.exec(
            "DELETE FROM reminders WHERE series_key=?1 AND status='pending'",
            &[Value::Text(key.into())],
        )?;
    }
    Ok(true)
}
