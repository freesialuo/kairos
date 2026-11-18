use crate::api::JsonFields as _;
use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    tz::TimeZone,
};
use serde_json::Value;

pub fn occurrence_date(start_local: &str) -> Option<Date> {
    let date = start_local
        .split_once('T')
        .map_or(start_local, |(date, _)| date);
    date.parse().ok()
}

#[derive(Clone)]
pub struct CalendarLink {
    event_id: i64,
    start: Option<Timestamp>,
    slot: Option<String>,
}

impl CalendarLink {
    fn parse(event: &str, start: Option<&str>, slot: Option<&str>) -> Option<Self> {
        let event_id = event.parse::<i64>().ok().filter(|&id| id > 0)?;
        let start = match start {
            Some(value) => Some(Timestamp::from_millisecond(value.parse().ok()?).ok()?),
            None => None,
        };
        let slot = match slot {
            Some("single") if start.is_some() => Some("single".into()),
            Some(value) if start.is_some() => {
                // Round-trip through civil types so only canonical slot text can enter a link.
                let canonical = if value.contains('T') {
                    format!("{:.0}", value.parse::<DateTime>().ok()?)
                } else {
                    value.parse::<Date>().ok()?.to_string()
                };
                if value != canonical {
                    return None;
                }
                Some(canonical)
            }
            Some(_) => return None,
            None => None,
        };
        Some(Self {
            event_id,
            start,
            slot,
        })
    }

    pub fn utc_day(&self) -> Option<Date> {
        self.start
            .map(|start| TimeZone::UTC.to_datetime(start).date())
    }

    pub fn matches(&self, occurrence: &Value) -> bool {
        occurrence.field("event_id").as_i64() == Some(self.event_id)
            && self.start.is_none_or(|start| {
                occurrence.field("start_utc").as_i64() == Some(start.as_millisecond())
            })
            && self
                .slot
                .as_deref()
                .is_none_or(|slot| occurrence.field("slot").as_str() == Some(slot))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn legacy_date_path(&self) -> Option<String> {
        self.start
            .is_none()
            .then(|| format!("/api/admin/events/{}/calendar-date", self.event_id))
    }
}

pub fn href(path: &'static str, target: Option<&CalendarLink>) -> String {
    let Some(target) = target else {
        return path.into();
    };
    let mut href = format!("{path}?event={}", target.event_id);
    if let Some(start) = target.start {
        href.push_str(&format!("&start={}", start.as_millisecond()));
    }
    if let Some(slot) = &target.slot {
        href.push_str(&format!("&slot={slot}"));
    }
    href
}

#[cfg(target_arch = "wasm32")]
pub fn current_target() -> Option<CalendarLink> {
    let location = web_sys::window()?.location();
    if !matches!(
        location.pathname().ok()?.as_str(),
        "/calendar" | "/login" | "/setup"
    ) {
        return None;
    }
    let params = web_sys::UrlSearchParams::new_with_str(&location.search().ok()?).ok()?;
    CalendarLink::parse(
        &params.get("event")?,
        params.get("start").as_deref(),
        params.get("slot").as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_the_link_across_fixed_local_routes() {
        let start = "1782865800000";
        let slot = "2026-06-30T17:30:00";
        let target = CalendarLink::parse("42", Some(start), Some(slot));
        assert_eq!(
            href("/login", target.as_ref()),
            format!("/login?event=42&start={start}&slot={slot}")
        );
        assert_eq!(
            href("/calendar", target.as_ref()),
            format!("/calendar?event=42&start={start}&slot={slot}")
        );
        assert_eq!(
            target.and_then(|link| link.utc_day()),
            Some(jiff::civil::date(2026, 7, 1))
        );
        assert_eq!(occurrence_date(slot), Some(jiff::civil::date(2026, 6, 30)));
        assert_eq!(
            href("/setup", CalendarLink::parse("42", None, None).as_ref()),
            "/setup?event=42"
        );
    }

    #[test]
    fn rejects_malformed_link_fields() {
        for event in ["0", "-1", "/evil.test", "42&next=//evil.test"] {
            assert!(CalendarLink::parse(event, None, None).is_none());
        }
        for start in [
            "abc",
            "1782865800000&next=//evil.test",
            "9223372036854775807",
        ] {
            assert!(CalendarLink::parse("42", Some(start), None).is_none());
        }
        for slot in ["single&next=//evil.test", "2026-06-31", "2026-07-01[a=b]"] {
            assert!(CalendarLink::parse("42", Some("1782865800000"), Some(slot)).is_none());
        }
    }

    #[test]
    fn picks_the_intended_slot_when_two_occurrences_share_a_start_time() {
        let target = CalendarLink::parse("42", Some("1782865800000"), Some("2026-07-01"));
        let selected =
            json!({"event_id": 42, "start_utc": 1782865800000_i64, "slot": "2026-07-01"});
        let other_slot =
            json!({"event_id": 42, "start_utc": 1782865800000_i64, "slot": "2026-07-02"});
        let other_event =
            json!({"event_id": 43, "start_utc": 1782865800000_i64, "slot": "2026-07-01"});
        assert!(target.as_ref().is_some_and(|link| link.matches(&selected)));
        assert!(
            target
                .as_ref()
                .is_some_and(|link| !link.matches(&other_slot))
        );
        assert!(
            target
                .as_ref()
                .is_some_and(|link| !link.matches(&other_event))
        );
        assert!(
            CalendarLink::parse("42", None, None).is_some_and(|link| link.matches(&other_slot))
        );
    }
}
