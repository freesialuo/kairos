//! Expand recurrence rules in civil time, then resolve each slot in its event zone.

use std::collections::HashSet;

use chrono::TimeZone as _;
use jiff::{SignedDuration, Span, Timestamp, civil::DateTime, tz::TimeZone};
use rrule::{RRule, RRuleSet, Tz, Unvalidated};

use crate::{
    calendar::model::{Anchor, EventRow, ExpandStatus, Occurrence, StoredEvent},
    time::zone::{WallValue, format_wall, parse_wall, utc_to_wall, wall_to_utc},
};

pub const EXPAND_BUDGET: u32 = 2_000;
pub const ANCHOR_LAG_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Budget {
    Slots(u32),
    Unlimited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpandOpts {
    pub anchor_before_utc: Option<i64>,
    pub budget: Budget,
}

impl Default for ExpandOpts {
    fn default() -> Self {
        Self {
            anchor_before_utc: None,
            budget: Budget::Slots(EXPAND_BUDGET),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandResult {
    pub occurrences: Vec<Occurrence>,
    pub anchor: Option<Anchor>,
    pub status: ExpandStatus,
    pub warning: Option<String>,
}

pub fn validate_rrule(rrule: &str) -> Result<(), String> {
    let start = Tz::UTC
        .with_ymd_and_hms(0, 1, 1, 0, 0, 0)
        .single()
        .ok_or("invalid validation date")?;
    parse_rule(rrule, None)?
        .0
        .validate(start)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub fn expand_series(
    rows: &[StoredEvent],
    from_utc: i64,
    to_utc: i64,
    user_tz: &str,
    opts: &ExpandOpts,
) -> ExpandResult {
    expand_with_event_windows(rows, user_tz, opts.budget, |_, _| {
        (from_utc..to_utc, opts.anchor_before_utc)
    })
}

pub(crate) fn all_day_start_floor(event: &EventRow, zone: &str, from: i64) -> i64 {
    // Step back in civil days so a daylight-saving transition cannot exclude an earlier start.
    let earliest = || {
        let today = utc_to_wall(from, zone).ok()?.date();
        let duration = Span::new().try_days(event.duration_sec / 86_400).ok()?;
        let day = today.checked_sub(duration).ok()?;
        wall_to_utc(day.at(0, 0, 0, 0), zone).ok()
    };
    earliest().unwrap_or(Timestamp::MIN.as_millisecond())
}

pub(crate) fn expand_overlapping_all_day(
    rows: &[StoredEvent],
    from: i64,
    to: i64,
    user_tz: &str,
) -> ExpandResult {
    let mut result = expand_with_event_windows(
        rows,
        user_tz,
        ExpandOpts::default().budget,
        |event, zone| {
            let start = if event.row.all_day {
                all_day_start_floor(&event.row, zone, from).min(from)
            } else {
                from
            };
            (start..to, None)
        },
    );
    result
        .occurrences
        .retain(|occ| occ.start_utc >= from || (occ.all_day && occ.end_utc > from));
    result
}

pub(crate) fn expand_with_event_windows(
    rows: &[StoredEvent],
    user_tz: &str,
    budget: Budget,
    window_for: impl Fn(&StoredEvent, &str) -> (std::ops::Range<i64>, Option<i64>),
) -> ExpandResult {
    let mut result = ExpandResult {
        occurrences: Vec::new(),
        anchor: None,
        status: ExpandStatus::Complete,
        warning: None,
    };
    let parent = rows.iter().find(|event| event.row.recurrence_id.is_none());
    if let Some(parent) = parent {
        let zone = parent.row.tzid.as_deref().unwrap_or(user_tz);
        let (window, anchor_before_utc) = window_for(parent, zone);
        if window.is_empty() {
            return result;
        }
        let suppressed: HashSet<_> = parent
            .row
            .exdates
            .iter()
            .map(String::as_str)
            .chain(
                rows.iter()
                    .filter_map(|row| row.row.recurrence_id.as_deref()),
            )
            .collect();

        let mut expanded = false;
        if let Some(rule) = parent.row.rrule.as_deref() {
            match recurrence_set(parent, rule, zone) {
                Ok((set, date_only, until_utc)) => {
                    let opts = ExpandOpts {
                        anchor_before_utc,
                        budget,
                    };
                    let recurrence = Recurrence {
                        event: parent,
                        zone,
                        window: &window,
                        opts: &opts,
                        date_only,
                        until_utc,
                        suppressed: &suppressed,
                    };
                    recurrence.expand(&set, rule, &mut result);
                    expanded = true;
                }
                Err(reason) => {
                    result.warning = Some(format!("Unsupported RRULE \"{rule}\": {reason}"));
                }
            }
        }

        if !expanded && let Some(occurrence) = single_occurrence(parent, "single", zone, &window) {
            result.occurrences.push(occurrence);
        }
    }

    if result.status == ExpandStatus::CatchingUp {
        return result;
    }
    result.occurrences.extend(rows.iter().filter_map(|event| {
        let slot = event.row.recurrence_id.as_deref()?;
        let zone = event.row.tzid.as_deref().unwrap_or(user_tz);
        single_occurrence(event, slot, zone, &window_for(event, zone).0)
    }));
    result
        .occurrences
        .sort_unstable_by(|a, b| (a.start_utc, &a.occ_key).cmp(&(b.start_utc, &b.occ_key)));
    result
}

fn recurrence_set(
    event: &StoredEvent,
    rule: &str,
    zone: &str,
) -> Result<(RRuleSet, bool, Option<i64>), String> {
    let value = parse_wall(&event.row.start_local).map_err(|error| error.to_string())?;
    let wall = as_datetime(value);
    let start = rule_start(wall).ok_or("start is out of range")?;
    let (rule, until_utc) = parse_rule(rule, event.row.tzid.as_ref().map(|_| zone))?;
    let set = rule.build(start).map_err(|error| error.to_string())?;
    Ok((set.limit(), matches!(value, WallValue::Date(_)), until_utc))
}

fn parse_rule(rule: &str, zone: Option<&str>) -> Result<(RRule<Unvalidated>, Option<i64>), String> {
    if rule.contains(['\r', '\n']) {
        return Err("expected a single RRULE value".into());
    }
    let mut normalized = String::with_capacity(rule.len());
    let mut until_utc = None;
    for part in rule.split_inclusive(';') {
        let field = part.strip_suffix(';').unwrap_or(part);
        match field.split_once('=') {
            Some((name, until)) if name.eq_ignore_ascii_case("UNTIL") => {
                normalized.push_str("UNTIL=");
                let (wall, cutoff) = until_wall(until, zone)?;
                until_utc = cutoff;
                normalized.push_str(&wall.strftime("%Y%m%dT%H%M%SZ").to_string());
            }
            _ => normalized.push_str(field),
        }
        if part.ends_with(';') {
            normalized.push(';');
        }
    }
    let normalized: RRule<Unvalidated> = normalized
        .parse()
        .map_err(|error: rrule::RRuleError| error.to_string())?;
    if normalized.get_interval() == 0 || normalized.get_count() == Some(0) {
        return Err("COUNT and INTERVAL must be positive".into());
    }
    if normalized.get_count().is_some() && normalized.get_until().is_some() {
        return Err("COUNT and UNTIL cannot be combined".into());
    }
    Ok((normalized, until_utc))
}

fn until_wall(until: &str, zone: Option<&str>) -> Result<(DateTime, Option<i64>), String> {
    let invalid = || format!("invalid UNTIL value {until:?}");
    let wall = match until.len() {
        8 => until
            .parse::<jiff::civil::Date>()
            .map(|date| date.at(0, 0, 0, 0)),
        15 => until.parse::<DateTime>(),
        16 if until.ends_with(['Z', 'z']) => {
            let instant = until.parse::<Timestamp>().map_err(|_| invalid())?;
            let Some(zone) = zone else {
                return Ok((TimeZone::UTC.to_datetime(instant), None));
            };
            let cutoff = instant.as_millisecond();
            let wall = utc_to_wall(cutoff, zone).map_err(|error| error.to_string())?;
            let compatible = wall_to_utc(wall, zone).map_err(|error| error.to_string())?;
            // A cutoff in the second half of an overlap still includes
            // later wall slots resolved to the first half. Keep them in the
            // iterator and apply the actual UTC cutoff after resolution.
            let wall = wall
                .checked_add(SignedDuration::from_millis(cutoff - compatible))
                .map_err(|error| error.to_string())?;
            return Ok((wall, Some(cutoff)));
        }
        _ => return Err(invalid()),
    };
    wall.map(|wall| (wall, None)).map_err(|_| invalid())
}

fn rule_start(wall: DateTime) -> Option<chrono::DateTime<Tz>> {
    let instant = jiff::tz::Offset::UTC.to_timestamp(wall).ok()?;
    Tz::UTC.timestamp_opt(instant.as_second(), 0).single()
}

fn as_datetime(value: WallValue) -> DateTime {
    match value {
        WallValue::Timed(wall) => wall,
        WallValue::Date(date) => date.at(0, 0, 0, 0),
    }
}

fn single_occurrence(
    event: &StoredEvent,
    slot: &str,
    zone: &str,
    window: &std::ops::Range<i64>,
) -> Option<Occurrence> {
    let wall = as_datetime(parse_wall(&event.row.start_local).ok()?);
    let start_utc = match event.row.initial_start_utc {
        Some(at) => at,
        None => wall_to_utc(wall, zone).ok()?,
    };
    if !window.contains(&start_utc) {
        return None;
    }
    occurrence(event, slot, wall, start_utc, zone)
}

fn occurrence(
    event: &StoredEvent,
    slot: &str,
    wall: DateTime,
    start_utc: i64,
    zone: &str,
) -> Option<Occurrence> {
    let row = &event.row;
    let end_utc = if row.all_day {
        let duration = Span::new().try_days(row.duration_sec / 86_400).ok()?;
        let end = wall.date().checked_add(duration).ok()?;
        wall_to_utc(end.at(0, 0, 0, 0), zone).ok()?
    } else {
        start_utc.checked_add(row.duration_sec.checked_mul(1_000)?)?
    };
    Some(Occurrence {
        event_id: event.id,
        series_key: row.series_key.clone(),
        occ_key: format!("{}|{slot}", row.series_key),
        slot: slot.into(),
        start_utc,
        end_utc,
        all_day: row.all_day,
    })
}

struct Recurrence<'a> {
    event: &'a StoredEvent,
    zone: &'a str,
    window: &'a std::ops::Range<i64>,
    opts: &'a ExpandOpts,
    date_only: bool,
    until_utc: Option<i64>,
    suppressed: &'a HashSet<&'a str>,
}

impl Recurrence<'_> {
    fn resume(&self, set: &RRuleSet, raw_rule: &str) -> Option<(u32, RRuleSet)> {
        if self.opts.budget == Budget::Unlimited {
            return None;
        }
        let anchor = self.event.expand_anchor.as_ref()?;
        if anchor.hash != self.event.hash {
            return None;
        }
        let rule = set.get_rrule().first()?;
        if !rule.get_by_set_pos().is_empty() {
            return None;
        }
        let wall = as_datetime(parse_wall(&anchor.slot).ok()?);
        let anchor_at = if anchor.slot == self.event.row.start_local {
            self.event.row.initial_start_utc
        } else {
            None
        };
        let anchor_at = match anchor_at {
            Some(at) => at,
            None => wall_to_utc(wall, self.zone).ok()?,
        };
        if anchor_at > self.window.start {
            return None;
        }
        let start = rule_start(wall)?;
        if start < *set.get_dt_start() {
            return None;
        }
        let (mut rebased, _) =
            parse_rule(raw_rule, self.event.row.tzid.as_ref().map(|_| self.zone)).ok()?;
        if let Some(count) = rule.get_count() {
            let remaining = count.checked_sub(anchor.index)?;
            if remaining == 0 {
                return None;
            }
            rebased = rebased.count(remaining);
        }
        let rebased = rebased.build(start).ok()?.limit();
        if rebased.into_iter().next() != Some(start) {
            return None;
        }
        Some((anchor.index, rebased))
    }

    fn anchor(&self, slot: String, index: u32) -> Anchor {
        Anchor {
            slot,
            index,
            hash: self.event.hash.clone(),
        }
    }

    fn expand(&self, set: &RRuleSet, raw_rule: &str, result: &mut ExpandResult) {
        let resumed = self.resume(set, raw_rule);
        let (first_index, set) = resumed
            .as_ref()
            .map_or((0, set), |(index, set)| (*index, set));
        result.anchor = resumed.as_ref().and(self.event.expand_anchor.clone());

        let mut remaining = match self.opts.budget {
            Budget::Slots(slots) => Some(slots),
            Budget::Unlimited => None,
        };
        let mut furthest = None;
        let mut max_start = i64::MIN;
        let can_catch_up = self.opts.budget != Budget::Slots(0)
            && set
                .get_rrule()
                .first()
                .is_some_and(|rule| rule.get_by_set_pos().is_empty());
        let end = self
            .until_utc
            .map_or(self.window.end, |until| self.window.end.min(until + 1));
        let initial_start = self.event.row.initial_start_utc.and_then(|at| {
            parse_wall(&self.event.row.start_local)
                .ok()
                .map(|value| (as_datetime(value), at))
        });
        for (index, slot) in set.into_iter().enumerate() {
            let Ok(instant) = Timestamp::from_millisecond(slot.timestamp_millis()) else {
                break;
            };
            let wall = TimeZone::UTC.to_datetime(instant);
            let Ok(compatible) = wall_to_utc(wall, self.zone) else {
                break;
            };
            let start = initial_start
                .filter(|(initial_wall, _)| *initial_wall == wall)
                .map_or(compatible, |(_, at)| at);

            // A slot shifted forward out of a gap can overtake an existing slot.
            // Only an existing wall slot is a safe end of the UTC window.
            if compatible >= end && start >= end && utc_to_wall(compatible, self.zone) == Ok(wall) {
                break;
            }
            // The anchor's index still counts, but replaying a slot already
            // before the window should not consume the only available budget.
            if index == 0 && resumed.is_some() && start < self.window.start {
                continue;
            }
            if let Some(slots) = remaining.as_mut() {
                if *slots == 0 {
                    if max_start < self.window.start && can_catch_up {
                        result.status = ExpandStatus::CatchingUp;
                        if let Some((slot, index)) = furthest {
                            result.anchor = Some(self.anchor(slot, index));
                        }
                    } else {
                        result.status = ExpandStatus::TooDense;
                        result.warning = Some(format!(
                            "Recurrence too dense; only the first {} occurrences in the window are scheduled",
                            result.occurrences.len()
                        ));
                    }
                    break;
                }
                *slots -= 1;
            }

            let stored_wall = if self.date_only {
                WallValue::Date(wall.date())
            } else {
                WallValue::Timed(wall)
            };
            let slot = format_wall(&stored_wall);
            let in_order = max_start < start;
            max_start = max_start.max(start);
            if let Ok(index) = u32::try_from(index)
                && let Some(index) = first_index.checked_add(index)
            {
                if in_order
                    && self
                        .opts
                        .anchor_before_utc
                        .is_some_and(|cutoff| max_start < cutoff)
                    && result
                        .anchor
                        .as_ref()
                        .is_none_or(|anchor| anchor.index < index)
                {
                    result.anchor = Some(self.anchor(slot.clone(), index));
                }
                furthest = Some((slot.clone(), index));
            }

            if (self.window.start..end).contains(&start)
                && !self.suppressed.contains(slot.as_str())
                && let Some(occurrence) = occurrence(self.event, &slot, wall, start, self.zone)
            {
                result.occurrences.push(occurrence);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    use super::*;
    use crate::calendar::model::Origin;

    fn ms(iso: &str) -> i64 {
        iso.parse::<Timestamp>().unwrap().as_millisecond()
    }

    fn parent(start: &str, zone: &str, rule: &str) -> StoredEvent {
        let row = EventRow {
            origin: Origin::Ics,
            source_id: Some(1),
            series_key: "ics:1:event".into(),
            uid: Some("event".into()),
            recurrence_id: None,
            external_id: None,
            title: "Event".into(),
            description: None,
            location: None,
            all_day: start.len() == 10,
            start_local: start.into(),
            initial_start_utc: None,
            tzid: Some(zone.into()),
            duration_sec: if start.len() == 10 { 86_400 } else { 3_600 },
            rrule: Some(rule.into()),
            exdates: Vec::new(),
            lead_minutes: None,
            valarm_minutes: Vec::new(),
        };
        StoredEvent {
            id: 1,
            hash: row.content_hash(),
            row,
            updated_at: 0,
            expand_anchor: None,
            expand_status: ExpandStatus::Complete,
            expand_warning: None,
        }
    }

    fn override_row(event: &StoredEvent, slot: &str, start: &str) -> StoredEvent {
        let mut event = event.clone();
        event.id = 2;
        event.row.recurrence_id = Some(slot.into());
        event.row.start_local = start.into();
        event.row.rrule = None;
        event.hash = event.row.content_hash();
        event
    }

    fn expand(rows: &[StoredEvent], from: &str, to: &str) -> ExpandResult {
        expand_series(rows, ms(from), ms(to), "UTC", &ExpandOpts::default())
    }

    fn slots(result: &ExpandResult) -> Vec<&str> {
        result.occurrences.iter().map(|o| o.slot.as_str()).collect()
    }

    #[test]
    fn weekly_stays_at_nine_across_dst() {
        let row = parent(
            "2026-10-19T09:00:00",
            "America/New_York",
            "FREQ=WEEKLY;COUNT=4",
        );
        let result = expand(&[row], "2026-10-19T00:00:00Z", "2026-11-16T00:00:00Z");
        assert_eq!(
            result
                .occurrences
                .iter()
                .map(|o| o.start_utc)
                .collect::<Vec<_>>(),
            [
                ms("2026-10-19T13:00:00Z"),
                ms("2026-10-26T13:00:00Z"),
                ms("2026-11-02T14:00:00Z"),
                ms("2026-11-09T14:00:00Z"),
            ],
        );
    }

    #[test]
    fn exdate_and_override_suppress_slots() {
        let mut row = parent("2026-11-20T09:00:00", "UTC", "FREQ=DAILY;COUNT=4");
        row.row.exdates.push("2026-11-21T09:00:00".into());
        let moved = override_row(&row, "2026-11-22T09:00:00", "2026-11-23T09:00:00");
        let result = expand(
            &[moved, row],
            "2026-11-20T00:00:00Z",
            "2026-11-24T00:00:00Z",
        );
        assert_eq!(
            slots(&result),
            [
                "2026-11-20T09:00:00",
                "2026-11-22T09:00:00",
                "2026-11-23T09:00:00",
            ]
        );
        let moved = result.occurrences.iter().find(|o| o.event_id == 2).unwrap();
        assert_eq!(moved.occ_key, "ics:1:event|2026-11-22T09:00:00");
        assert_eq!(moved.start_utc, ms("2026-11-23T09:00:00Z"));
    }

    #[test]
    fn orphan_override_expands_alone() {
        let row = parent("2026-11-20T09:00:00", "Asia/Kolkata", "FREQ=DAILY");
        let moved = override_row(&row, "2026-11-22T09:00:00", "2026-11-25T09:00:00");
        let result = expand(&[moved], "2026-11-25T00:00:00Z", "2026-11-26T00:00:00Z");
        assert_eq!(slots(&result), ["2026-11-22T09:00:00"]);
        assert_eq!(
            result.occurrences.first().unwrap().start_utc,
            ms("2026-11-25T03:30:00Z")
        );
    }

    #[test]
    fn count_includes_exdated_slots() {
        let mut row = parent("2026-11-20T09:00:00", "UTC", "FREQ=DAILY;COUNT=5");
        row.row.exdates.push("2026-11-21T09:00:00".into());
        let result = expand(&[row], "2026-11-20T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(
            slots(&result),
            [
                "2026-11-20T09:00:00",
                "2026-11-22T09:00:00",
                "2026-11-23T09:00:00",
                "2026-11-24T09:00:00",
            ]
        );
    }

    #[test]
    fn until_in_utc_on_zoned_rule() {
        let row = parent(
            "2026-11-22T23:30:00",
            "Europe/Berlin",
            "FREQ=DAILY;UNTIL=20261124T223000Z",
        );
        let result = expand(&[row], "2026-11-22T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(
            slots(&result),
            [
                "2026-11-22T23:30:00",
                "2026-11-23T23:30:00",
                "2026-11-24T23:30:00",
            ]
        );
    }

    #[test]
    fn utc_until_excludes_a_shifted_gap_slot_after_the_cutoff() {
        let row = parent(
            "2026-03-08T02:30:00",
            "America/New_York",
            "FREQ=MINUTELY;INTERVAL=15;UNTIL=20260308T071000Z",
        );
        let result = expand(&[row], "2026-03-08T00:00:00Z", "2026-03-09T00:00:00Z");
        assert_eq!(slots(&result), ["2026-03-08T03:00:00"]);
    }

    #[test]
    fn utc_until_in_later_overlap_includes_the_first_pass() {
        let row = parent(
            "2026-11-01T01:00:00",
            "America/New_York",
            "FREQ=MINUTELY;INTERVAL=15;UNTIL=20261101T061500Z",
        );
        let result = expand(&[row], "2026-11-01T00:00:00Z", "2026-11-02T00:00:00Z");
        assert_eq!(
            slots(&result),
            [
                "2026-11-01T01:00:00",
                "2026-11-01T01:15:00",
                "2026-11-01T01:30:00",
                "2026-11-01T01:45:00",
            ]
        );
    }

    #[test]
    fn anchor_resumes_with_rebased_count() {
        let mut row = parent("2026-11-20T09:00:00", "UTC", "FREQ=DAILY;COUNT=5");
        row.row.exdates.push("2026-11-22T09:00:00".into());
        row.hash = row.row.content_hash();
        let from = ms("2026-11-23T00:00:00Z");
        let to = ms("2026-12-01T00:00:00Z");
        let full = expand_series(
            std::slice::from_ref(&row),
            from,
            to,
            "UTC",
            &ExpandOpts {
                anchor_before_utc: Some(from),
                budget: Budget::Unlimited,
            },
        );
        assert_eq!(full.anchor.as_ref().unwrap().slot, "2026-11-22T09:00:00");
        assert_eq!(full.anchor.as_ref().unwrap().index, 2);
        row.expand_anchor = full.anchor.clone();
        let resumed = expand_series(&[row], from, to, "UTC", &ExpandOpts::default());
        assert_eq!(resumed.occurrences, full.occurrences);
        assert_eq!(resumed.status, ExpandStatus::Complete);
    }

    #[test]
    fn anchor_ignored_on_hash_mismatch() {
        let mut row = parent("2026-11-20T09:00:00", "UTC", "FREQ=DAILY;COUNT=5");
        row.expand_anchor = Some(Anchor {
            slot: "2026-11-22T09:00:00".into(),
            index: 4,
            hash: "outdated".into(),
        });
        let actual = expand(&[row], "2026-11-23T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(
            slots(&actual),
            ["2026-11-23T09:00:00", "2026-11-24T09:00:00"]
        );
    }

    #[test]
    fn bysetpos_ignores_anchor() {
        let mut row = parent(
            "2026-10-01T09:00:00",
            "UTC",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1;COUNT=3",
        );
        row.expand_anchor = Some(Anchor {
            slot: "2026-11-02T09:00:00".into(),
            index: 2,
            hash: row.hash.clone(),
        });
        let actual = expand(&[row], "2026-11-03T00:00:00Z", "2027-01-01T00:00:00Z");
        assert_eq!(slots(&actual), ["2026-12-01T09:00:00"]);
    }

    #[test]
    fn bysetpos_reports_exhaustion_without_a_resumable_anchor() {
        let row = parent(
            "2010-01-01T00:00:00",
            "UTC",
            "FREQ=DAILY;BYHOUR=0,1;BYSETPOS=1",
        );
        let actual = expand_series(
            &[row],
            ms("2026-11-20T00:00:00Z"),
            ms("2026-11-21T00:00:00Z"),
            "UTC",
            &ExpandOpts {
                budget: Budget::Slots(2),
                ..ExpandOpts::default()
            },
        );
        assert_eq!(actual.status, ExpandStatus::TooDense);
        assert!(actual.anchor.is_none());
        assert!(actual.warning.is_some());
    }

    #[test]
    fn ancient_hourly_series_catches_up() {
        let row = parent("2010-01-01T00:00:00", "UTC", "FREQ=HOURLY");
        let actual = expand(&[row], "2026-11-20T08:00:00Z", "2027-01-19T08:00:00Z");
        assert_eq!(actual.status, ExpandStatus::CatchingUp);
        assert_eq!(actual.anchor.unwrap().index, EXPAND_BUDGET - 1);
        assert!(actual.occurrences.is_empty());
    }

    #[test]
    fn minutely_rule_is_too_dense() {
        let row = parent("2026-11-20T08:00:00", "UTC", "FREQ=MINUTELY");
        let actual = expand(&[row], "2026-11-20T08:00:00Z", "2027-01-19T08:00:00Z");
        assert_eq!(actual.status, ExpandStatus::TooDense);
        assert_eq!(actual.occurrences.len(), EXPAND_BUDGET as usize);
        assert_eq!(
            actual.warning.as_deref(),
            Some(
                "Recurrence too dense; only the first 2000 occurrences in the window are scheduled"
            )
        );
    }

    #[test]
    fn unsupported_rule_is_non_recurring_with_warning() {
        let row = parent("2026-11-20T09:00:00", "UTC", "FREQ=EVERYOTHERWEEKEND");
        let actual = expand(&[row], "2026-11-20T00:00:00Z", "2026-12-01T00:00:00Z");
        assert_eq!(slots(&actual), ["single"]);
        assert!(
            actual
                .warning
                .unwrap()
                .starts_with("Unsupported RRULE \"FREQ=EVERYOTHERWEEKEND\": ")
        );
        assert!(validate_rrule("FREQ=DAILY;BYMONTH=16").is_err());
        assert!(validate_rrule("FREQ=DAILY;UNTIL=20261122T090000").is_ok());
        assert!(validate_rrule("FREQ=DAILY;UNTIL=20261122").is_ok());
        assert!(validate_rrule("FREQ=DAILY;COUNT=1;UNTIL=20261122").is_err());
        assert!(validate_rrule("FREQ=DAILY;COUNT=0").is_err());
        assert!(validate_rrule("FREQ=DAILY\nRDATE:20261122T090000Z").is_err());
    }

    #[test]
    fn all_day_duration_is_counted_in_wall_days() {
        let row = parent("2026-03-08", "America/New_York", "FREQ=DAILY;COUNT=1");
        let actual = expand(&[row], "2026-03-08T00:00:00Z", "2026-03-10T00:00:00Z");
        assert_eq!(slots(&actual), ["2026-03-08"]);
        let occurrence = actual.occurrences.first().unwrap();
        assert_eq!(occurrence.start_utc, ms("2026-03-08T05:00:00Z"));
        assert_eq!(occurrence.end_utc, ms("2026-03-09T04:00:00Z"));
    }

    #[test]
    fn shifted_gap_slot_does_not_hide_a_later_wall_slot_in_window() {
        let row = parent(
            "2026-03-08T02:30:00",
            "America/New_York",
            "FREQ=MINUTELY;INTERVAL=15",
        );
        let actual = expand(&[row], "2026-03-08T07:00:00Z", "2026-03-08T07:15:00Z");
        assert_eq!(slots(&actual), ["2026-03-08T03:00:00"]);
    }

    #[test]
    fn rfc5545_examples() {
        // First dates of the rules in RFC 5545, section 3.8.5.3.
        let cases = [
            (
                "1997-09-02",
                "FREQ=DAILY;UNTIL=19971224T000000Z",
                &["1997-09-02", "1997-09-03", "1997-09-04"][..],
            ),
            (
                "1997-09-02",
                "FREQ=DAILY;INTERVAL=2",
                &["1997-09-02", "1997-09-04", "1997-09-06"],
            ),
            (
                "1997-09-02",
                "FREQ=DAILY;INTERVAL=10;COUNT=5",
                &[
                    "1997-09-02",
                    "1997-09-12",
                    "1997-09-22",
                    "1997-10-02",
                    "1997-10-12",
                ],
            ),
            (
                "1998-01-01",
                "FREQ=YEARLY;UNTIL=20000131T140000Z;BYMONTH=1;BYDAY=SU,MO,TU,WE,TH,FR,SA",
                &["1998-01-01", "1998-01-02", "1998-01-03"],
            ),
            (
                "1998-01-01",
                "FREQ=DAILY;UNTIL=20000131T140000Z;BYMONTH=1",
                &["1998-01-01", "1998-01-02", "1998-01-03"],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;UNTIL=19971224T000000Z",
                &["1997-09-02", "1997-09-09", "1997-09-16"],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;INTERVAL=2;WKST=SU",
                &["1997-09-02", "1997-09-16", "1997-09-30"],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;UNTIL=19971007T000000Z;WKST=SU;BYDAY=TU,TH",
                &["1997-09-02", "1997-09-04", "1997-09-09"],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;COUNT=10;WKST=SU;BYDAY=TU,TH",
                &["1997-09-02", "1997-09-04", "1997-09-09"],
            ),
            (
                "1997-09-01",
                "FREQ=WEEKLY;INTERVAL=2;UNTIL=19971224T000000Z;WKST=SU;BYDAY=MO,WE,FR",
                &["1997-09-01", "1997-09-03", "1997-09-05", "1997-09-15"],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;INTERVAL=2;COUNT=8;WKST=SU;BYDAY=TU,TH",
                &["1997-09-02", "1997-09-04", "1997-09-16"],
            ),
            (
                "1997-09-05",
                "FREQ=MONTHLY;UNTIL=19971224T000000Z;BYDAY=1FR",
                &["1997-09-05", "1997-10-03", "1997-11-07", "1997-12-05"],
            ),
            (
                "1997-09-07",
                "FREQ=MONTHLY;INTERVAL=2;COUNT=10;BYDAY=1SU,-1SU",
                &["1997-09-07", "1997-09-28", "1997-11-02"],
            ),
            (
                "1997-09-02",
                "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=2,15",
                &["1997-09-02", "1997-09-15", "1997-10-02"],
            ),
            (
                "1997-09-10",
                "FREQ=MONTHLY;INTERVAL=18;COUNT=10;BYMONTHDAY=10,11,12,13,14,15",
                &["1997-09-10", "1997-09-11", "1997-09-12"],
            ),
            (
                "1997-09-02",
                "FREQ=MONTHLY;INTERVAL=2;BYDAY=TU",
                &["1997-09-02", "1997-09-09", "1997-09-16"],
            ),
            (
                "1997-03-10",
                "FREQ=YEARLY;INTERVAL=2;COUNT=10;BYMONTH=1,2,3",
                &["1997-03-10", "1999-01-10", "1999-02-10"],
            ),
            (
                "1997-05-19",
                "FREQ=YEARLY;BYDAY=20MO",
                &["1997-05-19", "1998-05-18", "1999-05-17"],
            ),
            (
                "1997-05-12",
                "FREQ=YEARLY;BYWEEKNO=20;BYDAY=MO",
                &["1997-05-12", "1998-05-11", "1999-05-17"],
            ),
            (
                "1997-03-13",
                "FREQ=YEARLY;BYMONTH=3;BYDAY=TH",
                &["1997-03-13", "1997-03-20", "1997-03-27"],
            ),
            (
                "1997-06-05",
                "FREQ=YEARLY;BYDAY=TH;BYMONTH=6,7,8",
                &["1997-06-05", "1997-06-12", "1997-06-19"],
            ),
            (
                "1997-09-02",
                "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13",
                &["1998-02-13", "1998-03-13", "1998-11-13"],
            ),
            (
                "1997-09-13",
                "FREQ=MONTHLY;BYDAY=SA;BYMONTHDAY=7,8,9,10,11,12,13",
                &["1997-09-13", "1997-10-11", "1997-11-08"],
            ),
            (
                "1996-11-05",
                "FREQ=YEARLY;INTERVAL=4;BYMONTH=11;BYDAY=TU;BYMONTHDAY=2,3,4,5,6,7,8",
                &["1996-11-05", "2000-11-07", "2004-11-02"],
            ),
            (
                "1997-09-04",
                "FREQ=MONTHLY;COUNT=3;BYDAY=TU,WE,TH;BYSETPOS=3",
                &["1997-09-04", "1997-10-07", "1997-11-06"],
            ),
            (
                "1997-09-29",
                "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-2",
                &["1997-09-29", "1997-10-30", "1997-11-27"],
            ),
            (
                "1997-09-02",
                "FREQ=DAILY;COUNT=10",
                &["1997-09-02", "1997-09-03", "1997-09-04"][..],
            ),
            (
                "1997-09-02",
                "FREQ=WEEKLY;COUNT=10",
                &["1997-09-02", "1997-09-09", "1997-09-16"],
            ),
            (
                "1997-09-05",
                "FREQ=MONTHLY;COUNT=10;BYDAY=1FR",
                &["1997-09-05", "1997-10-03", "1997-11-07"],
            ),
            (
                "1997-09-22",
                "FREQ=MONTHLY;COUNT=6;BYDAY=-2MO",
                &["1997-09-22", "1997-10-20", "1997-11-17"],
            ),
            (
                "1997-09-28",
                "FREQ=MONTHLY;BYMONTHDAY=-3",
                &["1997-09-28", "1997-10-29", "1997-11-28"],
            ),
            (
                "1997-09-30",
                "FREQ=MONTHLY;COUNT=10;BYMONTHDAY=1,-1",
                &["1997-09-30", "1997-10-01", "1997-10-31"],
            ),
            (
                "1997-06-10",
                "FREQ=YEARLY;COUNT=10;BYMONTH=6,7",
                &["1997-06-10", "1997-07-10", "1998-06-10"],
            ),
            (
                "1997-01-01",
                "FREQ=YEARLY;INTERVAL=3;COUNT=10;BYYEARDAY=1,100,200",
                &["1997-01-01", "1997-04-10", "1997-07-19"],
            ),
            (
                "1997-08-05",
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=MO",
                &["1997-08-05", "1997-08-10", "1997-08-19", "1997-08-24"],
            ),
            (
                "1997-08-05",
                "FREQ=WEEKLY;INTERVAL=2;COUNT=4;BYDAY=TU,SU;WKST=SU",
                &["1997-08-05", "1997-08-17", "1997-08-19", "1997-08-31"],
            ),
            (
                "2007-01-15",
                "FREQ=MONTHLY;BYMONTHDAY=15,30;COUNT=5",
                &[
                    "2007-01-15",
                    "2007-01-30",
                    "2007-02-15",
                    "2007-03-15",
                    "2007-03-30",
                ],
            ),
        ];
        for (start, rule, dates) in cases {
            let mut row = parent(&format!("{start}T09:00:00"), "America/New_York", rule);
            if rule == "FREQ=MONTHLY;BYDAY=FR;BYMONTHDAY=13" {
                row.row.exdates.push(row.row.start_local.clone());
            }
            let window_start = ms(&format!("{start}T00:00:00Z"));
            let boundary = dates.last().unwrap();
            let window_end = ms(&format!("{boundary}T23:00:00Z"));
            let actual = expand_series(
                &[row],
                window_start,
                window_end,
                "UTC",
                &ExpandOpts::default(),
            );
            let expected: Vec<_> = dates
                .iter()
                .map(|date| format!("{date}T09:00:00"))
                .collect();
            assert_eq!(slots(&actual), expected, "{rule}");
        }
    }

    #[test]
    fn rfc5545_intraday_examples() {
        let cases = [
            // RFC Editor erratum 3883 corrects the original UTC cutoff.
            (
                "FREQ=HOURLY;INTERVAL=3;UNTIL=19970902T210000Z",
                &["09:00", "12:00", "15:00"][..],
            ),
            (
                "FREQ=MINUTELY;INTERVAL=15;COUNT=6",
                &["09:00", "09:15", "09:30", "09:45", "10:00", "10:15"],
            ),
            (
                "FREQ=MINUTELY;INTERVAL=90;COUNT=4",
                &["09:00", "10:30", "12:00", "13:30"],
            ),
            (
                "FREQ=DAILY;BYHOUR=9,10,11,12,13,14,15,16;BYMINUTE=0,20,40",
                &["09:00", "09:20", "09:40", "10:00", "10:20"],
            ),
            (
                "FREQ=MINUTELY;INTERVAL=20;BYHOUR=9,10,11,12,13,14,15,16",
                &["09:00", "09:20", "09:40", "10:00", "10:20"],
            ),
        ];
        for (rule, times) in cases {
            let row = parent("1997-09-02T09:00:00", "America/New_York", rule);
            let expected = times
                .iter()
                .map(|time| format!("1997-09-02T{time}:00"))
                .collect::<Vec<_>>();
            let actual = expand(&[row], "1997-09-02T00:00:00Z", "1997-09-03T00:00:00Z");
            let actual = slots(&actual);
            assert_eq!(actual.get(..expected.len()).unwrap(), expected, "{rule}");
        }
    }
}
