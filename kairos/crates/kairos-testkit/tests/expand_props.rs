use jiff::{ToSpan, civil::DateTime};
use kairos_core::{
    calendar::model::{EventRow, ExpandStatus, Origin, StoredEvent},
    ics::expand::{Budget, ExpandOpts, expand_series},
    time::zone::wall_to_utc,
};
use proptest::prelude::*;

const ZONES: [&str; 5] = [
    "America/New_York",
    "Europe/Berlin",
    "Australia/Lord_Howe",
    "Asia/Kolkata",
    "UTC",
];

proptest! {
    #[test]
    fn anchored_expansion_matches_full_scan(
        zone in prop::sample::select(ZONES.to_vec()),
        year in 2018..=2026_i16,
        start_offset in 0..365_i32,
        frequency in 0..4_u8,
        interval in 1..=3_u8,
        count in prop::option::of(1..=400_u32),
        until in prop::option::of(0..=1_200_i32),
        exclusions in prop::collection::vec(0..120_i32, 0..=3),
        window_start in 0..=1_200_i32,
        window_days in 1..=365_i32,
    ) {
        let first_day: DateTime = format!("{year}-01-01T09:00:00").parse().unwrap();
        let start = first_day.checked_add(start_offset.days()).unwrap();
        let freq = match frequency {
            0 => "FREQ=DAILY",
            1 => "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            2 => "FREQ=MONTHLY;BYMONTHDAY=31",
            _ => "FREQ=YEARLY",
        };
        let mut rule = format!("{freq};INTERVAL={interval}");
        if let Some(count) = count {
            rule.push_str(&format!(";COUNT={count}"));
        } else if let Some(until) = until {
            let end = start.checked_add(until.days()).unwrap();
            let instant = wall_to_utc(end, zone).unwrap();
            let timestamp = jiff::Timestamp::from_millisecond(instant).unwrap();
            rule.push_str(&format!(";UNTIL={}", timestamp.strftime("%Y%m%dT%H%M%SZ")));
        }
        let row = EventRow {
            origin: Origin::Ics,
            source_id: Some(1),
            series_key: "ics:1:generated".into(),
            uid: Some("generated".into()),
            recurrence_id: None,
            external_id: None,
            title: "Generated event".into(),
            description: None,
            location: None,
            all_day: false,
            start_local: start.to_string(),
            initial_start_utc: None,
            tzid: Some(zone.into()),
            duration_sec: 3_600,
            rrule: Some(rule.clone()),
            exdates: exclusions.into_iter()
                .map(|days| start.checked_add(days.days()).unwrap().to_string())
                .collect(),
            lead_minutes: None,
            valarm_minutes: Vec::new(),
        };
        let mut row = StoredEvent {
            id: 1,
            hash: row.content_hash(),
            row,
            updated_at: 0,
            expand_anchor: None,
            expand_status: ExpandStatus::Complete,
            expand_warning: None,
        };
        let start_utc = wall_to_utc(start, zone).unwrap();
        let from = wall_to_utc(start.checked_add(window_start.days()).unwrap(), zone).unwrap();
        let to = wall_to_utc(start.checked_add((window_start + window_days).days()).unwrap(), zone).unwrap();
        let previous = expand_series(std::slice::from_ref(&row), start_utc, from, zone, &ExpandOpts {
            anchor_before_utc: Some(from),
            budget: Budget::Unlimited,
        });
        row.expand_anchor = previous.anchor;
        let resumed = expand_series(std::slice::from_ref(&row), from, to, zone, &ExpandOpts::default());
        let full = expand_series(&[row], from, to, zone, &ExpandOpts {
            anchor_before_utc: None,
            budget: Budget::Unlimited,
        });
        prop_assert_eq!(resumed.status, ExpandStatus::Complete, "{}", rule);
        prop_assert_eq!(resumed.warning, None, "{}", rule);
        prop_assert_eq!(resumed.occurrences, full.occurrences, "{}", rule);
    }
}
