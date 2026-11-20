use crate::{
    CoreError, Db, Hub, Ports,
    calendar::reconcile::WINDOW_MS,
    db::settings::{del_meta, read_meta, set_meta},
    ports::{Alarm, Commit, Value},
    sched::{
        dispatch::dispatch_due,
        queue::{REMINDER_SCHEDULE_READY, SeriesFilter, enqueue_where, process_queue},
    },
    sources::sync::{send_source_alerts, sync_due_source},
    time::zone::utc_to_wall,
};
use futures::FutureExt;
use std::{future::Future, panic::AssertUnwindSafe};

const DAY: i64 = 86_400_000;
const WINDOW_ROLL_AT: &str = "last_window_roll_at";
pub const ERROR_FLOOR_MAX_MIN: i64 = 15;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Floor {
    Http,
    AlarmClean,
    AlarmError,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct AlarmReport {
    pub step_errors: Vec<(String, String)>,
    pub top_level_error: Option<String>,
}
#[derive(Debug, Default)]
pub struct StepOutcome {
    pub ok: u32,
    pub failed: u32,
    pub step_error: Option<String>,
}

fn number(db: &impl Db, key: &str) -> Result<Option<i64>, CoreError> {
    read_meta(db, key)?
        .map(|text| {
            text.parse()
                .map_err(|_| CoreError::Internal(format!("invalid meta:{key}")))
        })
        .transpose()
}

pub fn compute_next_alarm(db: &impl Db, now: i64) -> Result<i64, CoreError> {
    let maintenance =
        number(db, "last_maintenance_at")?.map_or(now, |last| last.saturating_add(DAY));
    let rows=db.exec(&format!("SELECT MIN(at) AS earliest FROM (
        SELECT MIN(next_attempt_at) AS at FROM reminders WHERE status='pending' AND {REMINDER_SCHEDULE_READY}
        UNION ALL SELECT MIN(not_before) FROM reconcile_queue
        UNION ALL SELECT MIN(COALESCE(last_sync_at+interval_min*60000,?1)) FROM sources WHERE enabled=1
        UNION ALL SELECT ?2)"),&[Value::Integer(now),Value::Integer(maintenance)])?;
    rows.first()
        .ok_or_else(|| CoreError::Internal("missing next alarm".into()))?
        .i64("earliest")
        .map_err(Into::into)
}

pub fn run_maintenance(db: &impl Db, now: i64) -> Result<(), CoreError> {
    db.transaction(|db| {
        let lower = number(db, WINDOW_ROLL_AT)?
            .filter(|&last| last <= now)
            .map_or(now, |last| now.max(last.saturating_add(WINDOW_MS)));
        let upper = now.saturating_add(WINDOW_MS);
        let local_date = |instant| {
            utc_to_wall(instant, "UTC")
                .map(|wall| wall.date().to_string())
                .map_err(|err| CoreError::Internal(err.to_string()))
        };
        // Include one-offs newly in range using date bounds broad enough for
        // any IANA time-zone offset. Exact eligibility is decided by reconciliation.
        enqueue_where(
            db,
            SeriesFilter::WindowRoll {
                from_local_date: local_date(lower.saturating_sub(DAY))?,
                before_local_date: local_date(upper.saturating_add(2 * DAY))?,
            },
            "roll",
            now,
        )?;
        let cutoff = [Value::Integer(now.saturating_sub(90 * DAY))];
        db.exec("DELETE FROM deliveries WHERE at<?1", &cutoff)?;
        db.exec(
            "DELETE FROM reminders WHERE status!='pending' AND fire_at<?1",
            &cutoff,
        )?;
        db.exec(
            "DELETE FROM sessions WHERE expires_at<=?1",
            &[Value::Integer(now)],
        )?;
        db.exec(
            "DELETE FROM auth_failures WHERE at<?1",
            &[Value::Integer(now.saturating_sub(3_600_000))],
        )?;
        set_meta(db, WINDOW_ROLL_AT, &now.to_string())?;
        set_meta(db, "last_maintenance_at", &now.to_string())?;
        del_meta(db, "last_maintenance_error")
    })
}

async fn maintenance<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome {
    let work = async {
        if number(&hub.db, "last_maintenance_at")?.is_some_and(|t| t.saturating_add(DAY) > now) {
            return Ok(());
        }
        set_meta(
            &hub.db,
            "last_maintenance_at",
            &now.saturating_sub(23 * 3_600_000).to_string(),
        )?;
        hub.commit.commit_point().await?;
        run_maintenance(&hub.db, now)
    };
    let error = match AssertUnwindSafe(work).catch_unwind().await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(panic_message(e)),
    };
    if let Some(message) = &error {
        let _ = set_meta(&hub.db, "last_maintenance_error", message);
    }
    StepOutcome {
        step_error: error,
        ..Default::default()
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown");
    format!("panic: {text}")
}

pub async fn run_step<F: Future<Output = StepOutcome>>(_name: &str, f: F) -> StepOutcome {
    match AssertUnwindSafe(f).catch_unwind().await {
        Ok(step) => step,
        Err(payload) => StepOutcome {
            step_error: Some(panic_message(payload)),
            ..Default::default()
        },
    }
}

impl AlarmReport {
    fn collect(&mut self, name: &str, step: StepOutcome) {
        if let Some(error) = step.step_error {
            self.step_errors.push((name.into(), error));
        }
    }
}

impl<P: Ports> Hub<P> {
    pub async fn run_alarm(&self) -> AlarmReport {
        let mut report = AlarmReport::default();
        let run = async {
            let now = self.now();
            report.collect(
                "dispatch",
                run_step("dispatch", dispatch_due(self, now)).await,
            );
            report.collect("queue", run_step("queue", process_queue(self, now)).await);
            report.collect(
                "source_alerts",
                run_step("source_alerts", send_source_alerts(self, now)).await,
            );
            report.collect("sync", run_step("sync", sync_due_source(self, now)).await);
            report.collect(
                "maintenance",
                run_step("maintenance", maintenance(self, now)).await,
            );
        };
        if let Err(payload) = AssertUnwindSafe(run).catch_unwind().await {
            report.top_level_error = Some(panic_message(payload));
        }
        let floor = if report.step_errors.is_empty() && report.top_level_error.is_none() {
            Floor::AlarmClean
        } else {
            Floor::AlarmError
        };
        match AssertUnwindSafe(self.rearm(floor)).catch_unwind().await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => report.step_errors.push(("rearm".into(), e.to_string())),
            Err(payload) => report
                .step_errors
                .push(("rearm".into(), panic_message(payload))),
        }
        let errors = report
            .step_errors
            .iter()
            .map(|(name, e)| format!("{name}: {e}"))
            .chain(report.top_level_error.clone())
            .collect::<Vec<_>>();
        if !errors.is_empty() {
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                set_meta(&self.db, "last_alarm_error", &errors.join(", "))
            }));
        }
        report
    }

    pub(crate) async fn rearm_after_http(&self) {
        // The mutation already took effect. Keep its response even if scheduling fails.
        let error = match AssertUnwindSafe(self.rearm(Floor::Http))
            .catch_unwind()
            .await
        {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(panic) => panic_message(panic),
        };
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            set_meta(
                &self.db,
                "last_alarm_error",
                &format!("HTTP alarm rearm: {error}"),
            )
        }));
    }

    pub async fn rearm(&self, floor: Floor) -> Result<(), CoreError> {
        let now = self.now();
        let calculation = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let min_delay = match floor {
                Floor::Http => 1000,
                Floor::AlarmClean => {
                    if number(&self.db, "alarm_error_streak")?.unwrap_or(0) != 0 {
                        set_meta(&self.db, "alarm_error_streak", "0")?;
                    }
                    1000
                }
                Floor::AlarmError => {
                    let streak = number(&self.db, "alarm_error_streak")?
                        .unwrap_or(0)
                        .clamp(0, i64::MAX - 1)
                        + 1;
                    set_meta(&self.db, "alarm_error_streak", &streak.to_string())?;
                    (1_i64 << (streak - 1).min(4)).min(ERROR_FLOOR_MAX_MIN) * 60_000
                }
            };
            Ok::<_, CoreError>(
                compute_next_alarm(&self.db, now)?.max(now.saturating_add(min_delay)),
            )
        }));
        let calculated =
            calculation.unwrap_or_else(|panic| Err(CoreError::Internal(panic_message(panic))));
        // Even a corrupt setting or failed work query must leave an alarm for a later retry.
        let target = calculated
            .as_ref()
            .copied()
            .unwrap_or_else(|_| now.saturating_add(ERROR_FLOOR_MAX_MIN * 60_000));
        if self.alarm.get().await? != Some(target) {
            self.alarm.set(target).await?;
        }
        if let Err(error) = calculated {
            let _ = set_meta(
                &self.db,
                "last_alarm_error",
                &format!("compute next alarm: {error}"),
            );
            return Err(error);
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogAction {
    AlarmPresent,
    Rearmed,
}

pub async fn watchdog<P: Ports>(hub: &Hub<P>) -> Result<WatchdogAction, CoreError> {
    if hub.alarm.get().await?.is_some() {
        return Ok(WatchdogAction::AlarmPresent);
    }
    hub.rearm(Floor::Http).await?;
    hub.db.transaction(|db| {
        set_meta(db, "last_watchdog_at", &hub.now().to_string())?;
        set_meta(
            db,
            "last_alarm_error",
            "alarm was missing; re-armed by watchdog",
        )
    })?;
    Ok(WatchdogAction::Rearmed)
}
