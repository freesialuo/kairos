use jiff::{ToSpan, civil::Date};
use kairos_types::api_v1::OccurrenceDto;
use std::collections::BTreeMap;

pub fn week_of(date: Date) -> [Date; 7] {
    let days = i64::from(date.weekday().to_monday_zero_offset());
    let first = date.saturating_sub(days.days());
    std::array::from_fn(|i| first.saturating_add((i as i64).days()))
}
pub fn month_grid(year: i16, month: i8) -> Vec<[Date; 7]> {
    let Ok(mut d) = Date::new(year, month, 1) else {
        return vec![];
    };
    let mut grid = vec![];
    loop {
        let week = week_of(d);
        let Some(last) = week.last().copied() else {
            break;
        };
        grid.push(week);
        let Ok(next) = last.tomorrow() else { break };
        if next.month() != month && grid.len() >= 5 {
            break;
        }
        d = next;
    }
    grid
}
pub fn place(
    occs: &[OccurrenceDto],
    first: Date,
    last: Date,
) -> BTreeMap<Date, Vec<&OccurrenceDto>> {
    let mut slots = BTreeMap::new();
    for occ in occs {
        let date = |s: &str| s.get(..10).and_then(|d| d.parse::<Date>().ok());
        if let Some(start) = date(&occ.start_local) {
            let mut d = start.max(first);
            let end = date(&occ.end_local).unwrap_or(start);
            if d > last || (d > start && (!occ.all_day || d >= end)) {
                continue;
            }
            loop {
                slots.entry(d).or_insert_with(Vec::new).push(occ);
                let Ok(next) = d.tomorrow() else { break };
                if next > last || !occ.all_day || next >= end {
                    break;
                }
                d = next;
            }
        }
    }
    slots
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn november_has_six_monday_first_weeks() {
        let weeks = month_grid(2026, 11);
        assert_eq!(weeks.len(), 6);
        assert_eq!(
            weeks
                .first()
                .and_then(|w| w.first())
                .map(ToString::to_string)
                .as_deref(),
            Some("2026-10-26")
        );
        assert_eq!(
            weeks
                .last()
                .and_then(|w| w.last())
                .map(ToString::to_string)
                .as_deref(),
            Some("2026-12-06")
        );
        let date = "2026-11-22".parse().ok();
        assert_eq!(
            date.map(week_of)
                .and_then(|w| w.first().map(ToString::to_string))
                .as_deref(),
            Some("2026-11-16")
        );
    }
    #[test]
    fn multiday_placement_excludes_end_date() {
        let event = OccurrenceDto {
            occ_key: "api:one|single".into(),
            origin: "api".into(),
            external_id: None,
            source: None,
            title: "Retreat".into(),
            location: None,
            all_day: true,
            start_utc: 0,
            end_utc: 0,
            start_local: "2026-11-20".into(),
            end_local: "2026-11-23".into(),
        };
        let events = [event];
        let middle = jiff::civil::date(2026, 11, 21);
        assert_eq!(
            place(&events, middle, middle)
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![middle]
        );
        let slots = place(&events, Date::MIN, Date::MAX);
        assert_eq!(
            slots.keys().map(ToString::to_string).collect::<Vec<_>>(),
            ["2026-11-20", "2026-11-21", "2026-11-22"]
        );
    }
}
