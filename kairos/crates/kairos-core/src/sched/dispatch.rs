use super::alarm::StepOutcome;
use crate::{
    CoreError, Db, Hub, Ports,
    db::settings::get_settings,
    notify::{
        bark::{load_channel, send_bark},
        message::{ReminderView, format_reminder},
    },
    ports::{Commit, Row, Value},
    time::zone::{utc_to_wall, wall_to_utc},
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

pub const DISPATCH_BATCH: usize = 20;
pub const MAX_ATTEMPTS: u32 = 4;
pub const LATE_THRESHOLD_MS: i64 = 120_000;
pub const STARTED_GRACE_MS: i64 = 120_000;

pub fn retry_delay_ms(attempt: u32) -> i64 {
    match attempt {
        0 | 1 => 60_000,
        2 => 300_000,
        _ => 900_000,
    }
}

pub fn already_started(start: i64, all_day: bool, duration_sec: i64, now: i64) -> bool {
    if all_day {
        now >= start.saturating_add(duration_sec.saturating_mul(1000))
    } else {
        now > start.saturating_add(STARTED_GRACE_MS)
    }
}

enum Outcome {
    Sent,
    Skipped,
    Gone,
    Failed(String),
    Retry(String),
}

async fn deliver<P: Ports>(
    hub: &Hub<P>,
    trace: &Row,
    now: i64,
    late: bool,
) -> Result<Outcome, CoreError> {
    let event_id = trace.i64("event_id")?;
    let events = hub.db.exec(
        "SELECT e.title,e.location,e.all_day,e.duration_sec,e.tzid,s.name AS source,
         s.notify,s.enabled FROM events e LEFT JOIN sources s ON s.id=e.source_id
         WHERE e.id=?1 AND e.series_key=?2",
        &[
            Value::Integer(event_id),
            Value::Text(trace.text("series_key")?),
        ],
    )?;
    let Some(event) = events.first() else {
        return Ok(Outcome::Gone);
    };
    if event.opt_i64("notify")? == Some(0) || event.opt_i64("enabled")? == Some(0) {
        return Ok(Outcome::Gone);
    }
    let attempt = trace.i64("attempts")?;
    if attempt > i64::from(MAX_ATTEMPTS) {
        return Ok(Outcome::Failed("last attempt was interrupted".into()));
    }
    let all_day = event.bool("all_day")?;
    let start = trace.i64("occ_start_utc")?;
    let settings = get_settings(&hub.db)?;
    let zone = event
        .opt_text("tzid")?
        .unwrap_or_else(|| settings.timezone.clone());
    let ended = if all_day {
        // Midnight-to-midnight spans can be 23 or 25 hours on DST transition days.
        let local = utc_to_wall(start, &zone).map_err(|e| CoreError::Internal(e.to_string()))?;
        let days = event.i64("duration_sec")? / 86_400;
        let end = local
            .checked_add(
                jiff::Span::new()
                    .try_days(days)
                    .map_err(|e| CoreError::Internal(e.to_string()))?,
            )
            .map_err(|e| CoreError::Internal(e.to_string()))?;
        now >= wall_to_utc(end, &zone).map_err(|e| CoreError::Internal(e.to_string()))?
    } else {
        already_started(start, false, 0, now)
    };
    if ended {
        return Ok(if attempt == 1 {
            Outcome::Skipped
        } else {
            Outcome::Failed("event already started".into())
        });
    }
    let Some(channel) = load_channel(&hub.db)? else {
        return Ok(Outcome::Failed("no channel configured".into()));
    };
    let title = event.text("title")?;
    let location = event.opt_text("location")?;
    let source = event.opt_text("source")?;
    let view = ReminderView {
        title: &title,
        location: location.as_deref(),
        source_name: source.as_deref(),
        all_day,
        occ_start_utc: start,
        zone: &settings.timezone,
        now,
        late,
        event_id,
    };
    let msg = format_reminder(&view, &hub.config.origin);
    Ok(match send_bark(&hub.http, &channel, &msg).await {
        Ok(()) => Outcome::Sent,
        Err(error) => Outcome::Retry(error.to_string()),
    })
}

fn record(
    db: &impl Db,
    trace: &Row,
    now: i64,
    late: bool,
    outcome: Outcome,
) -> Result<(), CoreError> {
    let key = trace.text("key")?;
    let attempt = trace.i64("attempts")?;
    let params = [
        Value::Text(key.clone()),
        Value::Integer(attempt),
        Value::Integer(trace.i64("fire_at")?),
    ];
    if matches!(outcome, Outcome::Gone) {
        db.exec("DELETE FROM reminders WHERE key=?1 AND attempts=?2 AND fire_at=?3 AND status='pending'", &params)?;
        return Ok(());
    }
    let (status, error) = match outcome {
        Outcome::Sent => ("sent", None),
        Outcome::Skipped => ("skipped", Some("event already started".into())),
        Outcome::Retry(e) if attempt < i64::from(MAX_ATTEMPTS) => ("pending", Some(e)),
        Outcome::Retry(e) | Outcome::Failed(e) => ("failed", Some(e)),
        Outcome::Gone => return Ok(()),
    };
    let error = error.map_or(Value::Null, Value::Text);
    let ok = status == "sent";
    db.exec(
        "UPDATE reminders SET status=?4,last_error=?5,sent_at=?6,late=?7
        WHERE key=?1 AND attempts=?2 AND fire_at=?3 AND status='pending'",
        &[
            params[0].clone(),
            params[1].clone(),
            params[2].clone(),
            Value::Text(status.into()),
            error.clone(),
            if ok { Value::Integer(now) } else { Value::Null },
            Value::Integer(i64::from(late)),
        ],
    )?;
    db.exec(
        "INSERT INTO deliveries(at,reminder_key,kind,channel,ok,error,summary)
        VALUES (?1,?2,'reminder','bark',?3,?4,?5)",
        &[
            Value::Integer(now),
            Value::Text(key),
            Value::Integer(i64::from(ok)),
            error,
            Value::Text(status.into()),
        ],
    )?;
    Ok(())
}

pub async fn dispatch_due<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome {
    let mut step = StepOutcome::default();
    let work = async {
        let due = hub.db.exec(
            "SELECT key,attempts FROM reminders WHERE status='pending' AND next_attempt_at<=?1
            ORDER BY next_attempt_at,key LIMIT ?2",
            &[Value::Integer(now), Value::Integer(DISPATCH_BATCH as i64)],
        )?;
        for item in due {
            let attempts = u32::try_from(item.i64("attempts")?)
                .map_err(|e| CoreError::Internal(e.to_string()))?;
            let trace = hub.db.exec("UPDATE reminders SET attempts=attempts+1,next_attempt_at=?2,last_error='interrupted'
                WHERE key=?1 AND status='pending' AND next_attempt_at<=?3 AND attempts=?4
                RETURNING key,attempts,fire_at,event_id,series_key,occ_start_utc", &[
                Value::Text(item.text("key")?),Value::Integer(now.saturating_add(retry_delay_ms(attempts.saturating_add(1)))),
                Value::Integer(now),Value::Integer(i64::from(attempts))])?;
            let Some(trace) = trace.first() else {
                continue;
            };
            hub.commit.commit_point().await?;
            let late = now.saturating_sub(trace.i64("fire_at")?) > LATE_THRESHOLD_MS;
            let outcome = match AssertUnwindSafe(deliver(hub, trace, now, late))
                .catch_unwind()
                .await
            {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(e)) => Outcome::Retry(e.to_string()),
                Err(_) => Outcome::Retry("panic during reminder delivery".into()),
            };
            if matches!(outcome, Outcome::Failed(_) | Outcome::Retry(_)) {
                step.failed += 1;
            } else {
                step.ok += 1;
            }
            hub.db
                .transaction(|db| record(db, trace, now, late, outcome))?;
        }
        Ok::<(), CoreError>(())
    };
    if let Err(error) = work.await {
        step.step_error = Some(error.to_string());
    }
    step
}
