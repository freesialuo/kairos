use crate::time::zone::utc_to_wall;
use url::form_urlencoded;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
    pub url: Option<String>,
}

pub struct ReminderView<'a> {
    pub title: &'a str,
    pub location: Option<&'a str>,
    pub source_name: Option<&'a str>,
    pub all_day: bool,
    pub occ_start_utc: i64,
    pub zone: &'a str,
    pub now: i64,
    pub late: bool,
    pub event_id: i64,
    pub slot: Option<&'a str>,
}

pub fn format_reminder(v: &ReminderView<'_>, origin: &str) -> Message {
    let title = if v.late {
        format!("(late) ⏰ {}", v.title)
    } else {
        format!("⏰ {}", v.title)
    };
    let mut body = if v.all_day {
        "Today · all day".into()
    } else {
        let time = utc_to_wall(v.occ_start_utc, v.zone)
            .or_else(|_| utc_to_wall(v.occ_start_utc, "UTC"))
            .map_or_else(
                |_| "--:--".into(),
                |local| local.strftime("%H:%M").to_string(),
            );
        let until_start = v.occ_start_utc.saturating_sub(v.now);
        if until_start <= 60_000 {
            format!("{time} · now")
        } else {
            let minutes = until_start / 60_000 + i64::from(until_start % 60_000 >= 30_000);
            format!("{time} · in {minutes} min")
        }
    };
    for detail in [v.location, v.source_name].into_iter().flatten() {
        body.push_str(" · ");
        body.push_str(detail);
    }
    let mut query = form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("event", &v.event_id.to_string())
        .append_pair("start", &v.occ_start_utc.to_string());
    if let Some(slot) = v.slot {
        query.append_pair("slot", slot);
    }
    Message {
        title,
        body,
        url: Some(format!(
            "{}/calendar?{}",
            origin.trim_end_matches('/'),
            query.finish()
        )),
    }
}

pub fn format_source_alert(source_name: &str, error: &str, origin: &str) -> Message {
    let error: String = error.chars().take(200).collect();
    Message {
        title: "⚠️ Source sync failing".into(),
        body: format!("{source_name}: {error}"),
        url: Some(format!("{}/sources", origin.trim_end_matches('/'))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    use super::*;

    const ORIGIN: &str = "https://kairos.test";

    fn view() -> ReminderView<'static> {
        let start = "2026-11-20T16:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        ReminderView {
            title: "Standup",
            location: Some("Room 2"),
            source_name: Some("Work"),
            all_day: false,
            occ_start_utc: start,
            zone: "America/Los_Angeles",
            now: start - 15 * 60_000,
            late: false,
            event_id: 42,
            slot: Some("2026-11-20T08:00:00"),
        }
    }

    #[test]
    fn formats_timed_in_n_min() {
        assert_eq!(
            format_reminder(&view(), ORIGIN),
            Message {
                title: "⏰ Standup".into(),
                body: "08:00 · in 15 min · Room 2 · Work".into(),
                url: Some(
                    "https://kairos.test/calendar?event=42&start=1795190400000&slot=2026-11-20T08%3A00%3A00"
                        .into(),
                ),
            }
        );
        let mut next = view();
        next.now = next.occ_start_utc - 60_001;
        next.location = None;
        next.source_name = None;
        assert_eq!(format_reminder(&next, ORIGIN).body, "08:00 · in 1 min");
    }

    #[test]
    fn formats_now_for_lead_zero() {
        let mut event = view();
        event.location = None;
        event.source_name = None;
        for offset in [-60_000, 0, 3_000] {
            event.now = event.occ_start_utc + offset;
            assert_eq!(format_reminder(&event, ORIGIN).body, "08:00 · now");
        }
    }

    #[test]
    fn formats_all_day() {
        let event = ReminderView {
            all_day: true,
            location: None,
            ..view()
        };
        assert_eq!(
            format_reminder(&event, ORIGIN).body,
            "Today · all day · Work"
        );
    }

    #[test]
    fn late_prefix() {
        let event = ReminderView {
            late: true,
            ..view()
        };
        assert_eq!(format_reminder(&event, ORIGIN).title, "(late) ⏰ Standup");
    }

    #[test]
    fn alert_truncates_error_to_200_chars() {
        let error = "雪".repeat(201);
        assert_eq!(
            format_source_alert("Work", &error, "https://kairos.test/"),
            Message {
                title: "⚠️ Source sync failing".into(),
                body: format!("Work: {}", "雪".repeat(200)),
                url: Some("https://kairos.test/sources".into()),
            }
        );
    }
}
