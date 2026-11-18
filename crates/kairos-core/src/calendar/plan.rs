use super::model::{EventRow, Occurrence};
use crate::{
    CoreError,
    sources::model::SourceRow,
    time::zone::{utc_to_wall, wall_to_utc},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leads {
    Muted,
    Minutes(Vec<u32>),
}

pub fn resolve_leads(row: &EventRow, source: Option<&SourceRow>, global_default: u32) -> Leads {
    if source.is_some_and(|source| !source.enabled || !source.notify) {
        return Leads::Muted;
    }
    let mut minutes = if let Some(explicit) = &row.lead_minutes {
        explicit.clone()
    } else if source.is_some_and(|source| source.honor_valarm) && !row.valarm_minutes.is_empty() {
        row.valarm_minutes.clone()
    } else {
        vec![
            source
                .and_then(|source| source.default_lead_min)
                .unwrap_or(global_default),
        ]
    };
    minutes.sort_unstable();
    minutes.dedup();
    if minutes.is_empty() {
        Leads::Muted
    } else {
        Leads::Minutes(minutes)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PlannedReminder {
    pub key: String,
    pub series_key: String,
    pub event_id: i64,
    pub occ_start_utc: i64,
    pub lead_min: Option<u32>,
    pub fire_at: i64,
}

/// Propagate bad stored times so reconciliation can roll back without withdrawing reminders.
pub fn plan_reminders(
    occs: &[Occurrence],
    leads: &Leads,
    all_day_time: &str,
    zone_of: &dyn Fn(i64) -> String,
) -> Result<Vec<PlannedReminder>, CoreError> {
    let Leads::Minutes(minutes) = leads else {
        return Ok(vec![]);
    };
    let mut planned = Vec::new();
    for occ in occs {
        let mut add = |suffix: String, lead_min, fire_at| {
            planned.push(PlannedReminder {
                key: format!("{}|{suffix}", occ.occ_key),
                series_key: occ.series_key.clone(),
                event_id: occ.event_id,
                occ_start_utc: occ.start_utc,
                lead_min,
                fire_at,
            })
        };
        if occ.all_day {
            let zone = zone_of(occ.event_id);
            let time = all_day_time
                .parse::<jiff::civil::Time>()
                .map_err(|e| CoreError::Internal(format!("invalid all-day reminder time: {e}")))?;
            let wall = utc_to_wall(occ.start_utc, &zone)
                .map_err(|e| CoreError::Internal(e.to_string()))?;
            let fire_at = wall_to_utc(wall.date().to_datetime(time), &zone)
                .map_err(|e| CoreError::Internal(e.to_string()))?;
            add("allday".into(), None, fire_at);
        } else {
            for &lead in minutes {
                let fire_at = occ
                    .start_utc
                    .checked_sub(i64::from(lead) * 60_000)
                    .ok_or_else(|| CoreError::Internal("reminder time out of range".into()))?;
                add(lead.to_string(), Some(lead), fire_at);
            }
        }
    }
    Ok(planned)
}
