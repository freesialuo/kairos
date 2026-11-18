use super::*;
use crate::{
    db::settings::{Settings, get_settings, put_settings},
    sched::queue::{SeriesFilter, enqueue_where},
    time::zone::is_valid_zone,
};

pub(super) fn handle<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    if req.method == Method::Get {
        return ok(get_settings(&hub.db)?);
    }
    if req.method != Method::Put {
        return Err(CoreError::NotFound);
    }
    let s: Settings = body(req)?;
    if !is_valid_zone(&s.timezone) {
        return Err(invalid("timezone"));
    }
    if s.default_lead_min > 10080 {
        return Err(invalid("default_lead_min"));
    }
    if s.all_day_time.len() != 5
        || s.all_day_time.get(2..3) != Some(":")
        || s.all_day_time.parse::<jiff::civil::Time>().is_err()
    {
        return Err(invalid("all_day_time"));
    }
    let queued = hub.db.transaction(|db| {
        let old = get_settings(db)?;
        if old == s {
            return Ok(0);
        }
        put_settings(db, &s)?;
        let filter = if old.default_lead_min != s.default_lead_min {
            SeriesFilter::All
        } else {
            SeriesFilter::FloatingOrAllDay
        };
        enqueue_where(db, filter, "settings", hub.now())
    })?;
    ok(json!({"queued":queued}))
}
