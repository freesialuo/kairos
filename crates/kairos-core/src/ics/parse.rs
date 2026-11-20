use std::{
    collections::{BTreeSet, HashMap},
    io::Cursor,
};

use super::expand::validate_rrule;
use crate::{
    calendar::model::{EventRow, Origin},
    time::zone::{WallValue, format_wall, is_valid_zone, parse_wall, utc_to_wall, wall_to_utc},
};
use ical::{
    IcalParser,
    parser::ical::component::{IcalAlarm, IcalEvent},
    property::Property,
};

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedFeed {
    pub rows: Vec<EventRow>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IcsError {
    NotCalendar,
    InvalidUtf8,
    InvalidEvent(&'static str),
}

pub fn parse_ics(bytes: &[u8], source_id: i64) -> Result<ParsedFeed, IcsError> {
    let mut bytes = unfold(bytes);
    std::str::from_utf8(&bytes).map_err(|_| IcsError::InvalidUtf8)?;
    normalize_components(&mut bytes)?;
    let mut calendars = IcalParser::new(Cursor::new(bytes)).peekable();
    if calendars.peek().is_none() {
        return Err(IcsError::NotCalendar);
    }
    let mut events = Vec::new();
    for calendar in calendars {
        let calendar = calendar.map_err(|_| IcsError::NotCalendar)?;
        events.extend(calendar.events);
    }

    // Overrides and cancellations can precede their masters in a feed.
    let mut masters: HashMap<&str, Master<'_>> = HashMap::new();
    for event in &events {
        let properties = &event.properties;
        if property(properties, "RECURRENCE-ID").is_none()
            && let Some(uid) = value(properties, "UID")
        {
            let master = masters.entry(uid).or_default();
            master.cancelled |= cancelled(properties);
            master.zone = property(properties, "DTSTART")
                .and_then(|start| explicit_zone(start, start.value.as_deref()?));
        }
    }

    let mut parsed = ParsedFeed {
        rows: Vec::new(),
        warnings: Vec::new(),
    };
    let mut cancelled_slots = HashMap::<&str, BTreeSet<String>>::new();
    for event in &events {
        let properties = &event.properties;
        let uid = value(properties, "UID")
            .filter(|uid| !uid.trim().is_empty())
            .ok_or(IcsError::InvalidEvent("missing event UID"))?;
        let master = masters.get(uid);
        if master.is_some_and(|master| master.cancelled) {
            continue;
        }
        let recurrence_id = match property(properties, "RECURRENCE-ID") {
            Some(recurrence) => {
                let slot = recurrence
                    .value
                    .as_deref()
                    .and_then(|raw| {
                        wall_in_zone(recurrence, raw, master.and_then(|master| master.zone))
                    })
                    .ok_or(IcsError::InvalidEvent("invalid event RECURRENCE-ID"))?;
                Some(format_wall(&slot))
            }
            None => None,
        };
        if cancelled(properties) {
            if master.is_some()
                && let Some(slot) = recurrence_id
            {
                cancelled_slots.entry(uid).or_default().insert(slot);
            }
        } else {
            let row = parse_event(event, source_id, uid, recurrence_id, &mut parsed.warnings)
                .ok_or(IcsError::InvalidEvent("missing or invalid event DTSTART"))?;
            parsed.rows.push(row);
        }
    }

    parsed.rows.retain_mut(|row| {
        let Some(slots) = row.uid.as_deref().and_then(|uid| cancelled_slots.get(uid)) else {
            return true;
        };
        if let Some(slot) = &row.recurrence_id {
            return !slots.contains(slot);
        }
        let existing = row
            .exdates
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let new = slots
            .iter()
            .filter(|slot| !existing.contains(slot.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        row.exdates.extend(new);
        true
    });
    Ok(parsed)
}

#[derive(Default)]
struct Master<'a> {
    zone: Option<&'a str>,
    cancelled: bool,
}

fn unfold(bytes: &[u8]) -> Vec<u8> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut unfolded = Vec::with_capacity(bytes.len());
    for line in bytes.split_inclusive(|&byte| byte == b'\n') {
        if let [b' ' | b'\t', continuation @ ..] = line
            && unfolded.last() == Some(&b'\n')
        {
            unfolded.pop();
            if unfolded.last() == Some(&b'\r') {
                unfolded.pop();
            }
            unfolded.extend_from_slice(continuation);
        } else {
            unfolded.extend_from_slice(line);
        }
    }
    unfolded
}

fn normalize_components(bytes: &mut [u8]) -> Result<(), IcsError> {
    // ical requires uppercase BEGIN/END markers and accepts a mismatched END.
    // Check the boundaries before treating the feed as a successful snapshot.
    let mut components = Vec::new();
    for line in bytes.split_mut(|&byte| byte == b'\n') {
        let Some(colon) = line.iter().position(|&byte| byte == b':') else {
            continue;
        };
        let Some((name, value)) = line.split_at_mut_checked(colon) else {
            continue;
        };
        let opening = name.eq_ignore_ascii_case(b"BEGIN");
        if !opening && !name.eq_ignore_ascii_case(b"END") {
            continue;
        }
        name.make_ascii_uppercase();
        value.make_ascii_uppercase();
        let component = value.trim_ascii_end();
        if opening {
            components.push(component);
        } else if components.pop() != Some(component) {
            return Err(IcsError::NotCalendar);
        }
    }
    if components.is_empty() {
        Ok(())
    } else {
        Err(IcsError::NotCalendar)
    }
}

fn parse_event(
    event: &IcalEvent,
    source_id: i64,
    uid: &str,
    recurrence_id: Option<String>,
    warnings: &mut Vec<String>,
) -> Option<EventRow> {
    let properties = &event.properties;
    let start = property(properties, "DTSTART")?;
    let raw_start = start.value.as_deref()?.trim();
    let wall = parse_ical_wall(start, raw_start)?;
    let all_day = matches!(wall, WallValue::Date(_));
    let title = value(properties, "SUMMARY")
        .map(unescape_text)
        .unwrap_or_else(|| "(untitled)".into());
    let tzid = explicit_zone(start, raw_start);
    if !all_day
        && tzid.is_none()
        && let Some(zone) = parameter(start, "TZID")
    {
        warnings.push(format!(
            r#"Unknown TZID "{zone}" on "{title}"; using your time zone"#
        ));
    }
    if property(properties, "RDATE").is_some() {
        warnings.push(format!(r#"RDATE ignored on "{title}""#));
    }
    let rrule = value(properties, "RRULE").map(|rule| {
        let rule = rule.strip_prefix("RRULE:").unwrap_or(rule);
        if let Err(reason) = validate_rrule(rule) {
            warnings.push(format!(r#"Unsupported RRULE "{rule}": {reason}"#));
        }
        rule.to_owned()
    });
    let exdates = properties
        .iter()
        .filter(|property| property.name.eq_ignore_ascii_case("EXDATE"))
        .filter_map(|property| Some((property, property.value.as_deref()?)))
        .flat_map(|(property, raw)| {
            raw.split(',')
                .filter_map(move |item| wall_in_zone(property, item, tzid))
        })
        .map(|date| format_wall(&date))
        .collect();
    let valarm_minutes = event
        .alarms
        .iter()
        .filter_map(alarm_minutes)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    Some(EventRow {
        origin: Origin::Ics,
        source_id: Some(source_id),
        series_key: format!("ics:{source_id}:{uid}"),
        uid: Some(uid.to_owned()),
        recurrence_id,
        external_id: None,
        title,
        description: value(properties, "DESCRIPTION").map(unescape_text),
        location: value(properties, "LOCATION").map(unescape_text),
        all_day,
        start_local: format_wall(&wall),
        initial_start_utc: None,
        tzid: tzid.map(str::to_owned),
        duration_sec: event_duration(properties, wall, tzid),
        rrule,
        exdates,
        lead_minutes: None,
        valarm_minutes,
    })
}

fn property<'a>(properties: &'a [Property], name: &str) -> Option<&'a Property> {
    properties
        .iter()
        .find(|item| item.name.eq_ignore_ascii_case(name))
}

fn value<'a>(properties: &'a [Property], name: &str) -> Option<&'a str> {
    property(properties, name)?.value.as_deref()
}

fn parameter<'a>(property: &'a Property, name: &str) -> Option<&'a str> {
    let (_, values) = property
        .params
        .as_ref()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?;
    values.first().map(String::as_str)
}

fn cancelled(properties: &[Property]) -> bool {
    value(properties, "STATUS").is_some_and(|status| status.eq_ignore_ascii_case("CANCELLED"))
}

fn explicit_zone<'a>(property: &'a Property, raw: &str) -> Option<&'a str> {
    if raw.ends_with(['Z', 'z']) {
        Some("UTC")
    } else if raw.len() == 8 {
        None
    } else {
        parameter(property, "TZID").filter(|zone| is_valid_zone(zone))
    }
}

fn parse_ical_wall(property: &Property, raw: &str) -> Option<WallValue> {
    let raw = raw.trim();
    let local = raw.strip_suffix(['Z', 'z']).unwrap_or(raw);
    let (date, tail) = local.split_at_checked(8)?;
    let date = format!("{}-{}-{}", date.get(..4)?, date.get(4..6)?, date.get(6..)?);
    if tail.is_empty() && raw == local {
        return parse_wall(&date).ok();
    }
    if parameter(property, "VALUE").is_some_and(|value| value.eq_ignore_ascii_case("DATE")) {
        return None;
    }
    let time = tail.strip_prefix(['T', 't'])?;
    if time.len() != 6 {
        return None;
    }
    let timed = format!(
        "{date}T{}:{}:{}",
        time.get(..2)?,
        time.get(2..4)?,
        time.get(4..)?
    );
    parse_wall(&timed).ok()
}

fn wall_in_zone(property: &Property, raw: &str, target_zone: Option<&str>) -> Option<WallValue> {
    let raw = raw.trim();
    let wall = parse_ical_wall(property, raw)?;
    match (wall, explicit_zone(property, raw), target_zone) {
        (WallValue::Timed(time), Some(source), Some(target))
            if !source.eq_ignore_ascii_case(target) =>
        {
            let instant = wall_to_utc(time, source).ok()?;
            Some(WallValue::Timed(utc_to_wall(instant, target).ok()?))
        }
        _ => Some(wall),
    }
}

fn event_duration(properties: &[Property], start: WallValue, zone: Option<&str>) -> i64 {
    let end = property(properties, "DTEND")
        .and_then(|end| wall_in_zone(end, end.value.as_deref()?, zone));
    let width = match (start, end) {
        (WallValue::Timed(start), Some(WallValue::Timed(end))) => Some(end.duration_since(start)),
        (WallValue::Date(start), Some(WallValue::Date(end))) => Some(end.duration_since(start)),
        _ => None,
    };
    width
        .map(|width| width.as_secs())
        .or_else(|| value(properties, "DURATION").and_then(duration_seconds))
        .filter(|seconds| *seconds >= 0)
        .unwrap_or(match start {
            WallValue::Date(_) => 86_400,
            WallValue::Timed(_) => 0,
        })
}

fn alarm_minutes(alarm: &IcalAlarm) -> Option<u32> {
    let trigger = property(&alarm.properties, "TRIGGER")?;
    if parameter(trigger, "RELATED").is_some_and(|related| !related.eq_ignore_ascii_case("START"))
        || parameter(trigger, "VALUE").is_some_and(|kind| !kind.eq_ignore_ascii_case("DURATION"))
    {
        return None;
    }
    let seconds = duration_seconds(trigger.value.as_deref()?)?;
    if seconds > 0 {
        return None;
    }
    u32::try_from(seconds.unsigned_abs() / 60).ok()
}

fn duration_seconds(value: &str) -> Option<i64> {
    let (negative, body) = match value.as_bytes().first() {
        Some(b'-') => (true, value.get(1..)?),
        Some(b'+') => (false, value.get(1..)?),
        _ => (false, value),
    };
    let body = body.strip_prefix(['P', 'p'])?;
    if let Some(weeks) = body.strip_suffix(['W', 'w']) {
        let total = duration_number(weeks)?.checked_mul(604_800)?;
        return Some(if negative { -total } else { total });
    }
    let (days, time) = body
        .split_once(['T', 't'])
        .map_or((body, None), |(days, time)| (days, Some(time)));
    let mut total = if days.is_empty() {
        time?;
        0
    } else {
        duration_number(days.strip_suffix(['D', 'd'])?)?.checked_mul(86_400)?
    };
    if let Some(time) = time {
        if time.is_empty() {
            return None;
        }
        let mut previous = 86_400;
        for part in time.split_inclusive(|character: char| character.is_ascii_alphabetic()) {
            let (digits, unit) = part.split_at_checked(part.len().checked_sub(1)?)?;
            let multiplier = match unit {
                "H" | "h" => 3_600,
                "M" | "m" => 60,
                "S" | "s" => 1,
                _ => return None,
            };
            if multiplier >= previous {
                return None;
            }
            previous = multiplier;
            total = total.checked_add(duration_number(digits)?.checked_mul(multiplier)?)?;
        }
    }
    Some(if negative { -total } else { total })
}

fn duration_number(value: &str) -> Option<i64> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn unescape_text(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            result.push(character);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => result.push('\n'),
            Some(escaped @ (',' | ';' | '\\')) => result.push(escaped),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{IcsError, ParsedFeed, parse_ics};
    use crate::calendar::model::Origin;

    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    macro_rules! fixture {
        ($name:literal) => {
            parse_ics(
                include_str!(concat!("../../../kairos-testkit/fixtures/", $name, ".ics"))
                    .as_bytes(),
                42,
            )
            .unwrap()
        };
    }

    #[test]
    fn parses_timed_tzid_event() {
        let feed = fixture!("timed-tzid");
        let row = &feed.rows[0];
        assert_eq!(feed.rows.len(), 1);
        assert_eq!(row.origin, Origin::Ics);
        assert_eq!(row.source_id, Some(42));
        assert_eq!(row.uid.as_deref(), Some("berlin"));
        assert_eq!(row.series_key, "ics:42:berlin");
        assert_eq!(row.recurrence_id, None);
        assert_eq!(row.external_id, None);
        assert_eq!(row.lead_minutes, None);
        assert!(!row.all_day);
        assert_eq!(row.tzid.as_deref(), Some("Europe/Berlin"));
        assert_eq!(row.start_local, "2026-11-23T09:00:00");
        assert_eq!(row.duration_sec, 3600);
        assert_eq!(row.title, "Meeting, review");
        assert_eq!(row.description.as_deref(), Some("First line\nSecond line"));
        assert_eq!(row.location.as_deref(), Some("East; Room 2"));
        assert!(feed.warnings.is_empty());
    }

    #[test]
    fn parses_utc_and_floating() {
        let feed = fixture!("utc-and-floating");
        assert_eq!(feed.rows.len(), 2);
        assert_eq!(feed.rows[0].tzid.as_deref(), Some("UTC"));
        assert_eq!(feed.rows[0].start_local, "2026-11-23T09:00:00");
        assert_eq!(feed.rows[0].duration_sec, 5400);
        assert_eq!(feed.rows[1].tzid, None);
        assert_eq!(feed.rows[1].start_local, "2026-11-23T09:00:00");
        assert_eq!(feed.rows[1].duration_sec, 0);
    }

    #[test]
    fn parses_all_day_default_duration() {
        let feed = fixture!("all-day");
        let row = &feed.rows[0];
        assert!(row.all_day);
        assert_eq!(row.start_local, "2026-12-25");
        assert_eq!(row.tzid, None);
        assert_eq!(row.duration_sec, 86_400);
        assert_eq!(feed.rows[1].duration_sec, 259_200);
    }

    #[test]
    fn unknown_tzid_falls_back_with_warning() {
        let feed = fixture!("unknown-tzid");
        assert_eq!(feed.rows[0].tzid, None);
        assert_eq!(
            feed.warnings,
            [r#"Unknown TZID "China Standard Time" on "Standup"; using your time zone"#]
        );
    }

    #[test]
    fn rrule_and_repeated_exdate() {
        let feed = fixture!("exdates");
        let row = &feed.rows[0];
        assert_eq!(row.rrule.as_deref(), Some("FREQ=WEEKLY;COUNT=4"));
        assert_eq!(
            row.exdates,
            [
                "2026-11-30T09:00:00",
                "2026-12-07T09:00:00",
                "2026-12-14T09:00:00"
            ]
        );
    }

    #[test]
    fn override_keeps_original_slot() {
        let feed = fixture!("override");
        let row = feed
            .rows
            .iter()
            .find(|row| row.recurrence_id.is_some())
            .unwrap();
        assert_eq!(feed.rows.len(), 2);
        assert_eq!(row.series_key, "ics:42:series");
        assert_eq!(row.recurrence_id.as_deref(), Some("2026-11-30T09:00:00"));
        assert_eq!(row.start_local, "2026-11-30T13:00:00");
        assert_eq!(row.tzid.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn cancelled_instance_becomes_exdate() {
        let feed = fixture!("cancelled-instance");
        assert_eq!(feed.rows.len(), 1);
        assert_eq!(feed.rows[0].recurrence_id, None);
        assert_eq!(feed.rows[0].exdates, ["2026-11-30T09:00:00"]);
    }

    #[test]
    fn cancelled_master_drops_series() {
        assert!(fixture!("cancelled-master").rows.is_empty());
    }

    #[test]
    fn orphan_cancelled_override_ignored() {
        assert!(fixture!("orphan-cancelled").rows.is_empty());
    }

    #[test]
    fn orphan_override_kept() {
        let feed = fixture!("orphan-override");
        assert_eq!(feed.rows.len(), 1);
        assert_eq!(
            feed.rows[0].recurrence_id.as_deref(),
            Some("2026-11-30T08:00:00")
        );
        assert_eq!(feed.rows[0].start_local, "2026-11-30T10:00:00");
    }

    #[test]
    fn thisandfuture_is_single_override() {
        assert_eq!(fixture!("range-this-and-future").rows.len(), 2);
        let normal = fixture!("override");
        let range = fixture!("range-this-and-future");
        let override_row = |feed: ParsedFeed| {
            feed.rows
                .into_iter()
                .find(|row| row.recurrence_id.is_some())
                .unwrap()
        };
        assert_eq!(override_row(range), override_row(normal));
    }

    #[test]
    fn valarm_relative_to_start_only() {
        assert_eq!(fixture!("valarm").rows[0].valarm_minutes, [0, 10, 60]);
    }

    #[test]
    fn rdate_warns() {
        let feed = fixture!("rdate");
        assert_eq!(feed.rows.len(), 1);
        assert_eq!(feed.warnings, [r#"RDATE ignored on "Dentist""#]);
    }

    #[test]
    fn unsupported_rrule_warns() {
        let feed = fixture!("unsupported-rrule");
        assert_eq!(
            feed.rows[0].rrule.as_deref(),
            Some("FREQ=EVERYOTHERWEEKEND")
        );
        assert_eq!(feed.warnings.len(), 1);
        assert!(feed.warnings[0].starts_with(r#"Unsupported RRULE "FREQ=EVERYOTHERWEEKEND": "#));
    }

    #[test]
    fn html_page_is_not_calendar() {
        assert_eq!(
            parse_ics(b"<!doctype html><html>", 1),
            Err(IcsError::NotCalendar)
        );
    }

    #[test]
    fn crlf_bom_and_folded_multibyte_title() {
        let bytes = b"\xEF\xBB\xBFBEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261123T090000Z\r\nSUMMARY:\xE4\xBC\x9A\xE8\r\n \xAE\xAE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert_eq!(parse_ics(bytes, 1).unwrap().rows[0].title, "会议");
    }

    #[test]
    fn invalid_utf8_is_error() {
        assert_eq!(
            parse_ics(b"BEGIN:VCALENDAR\r\nSUMMARY:\xFF\r\nEND:VCALENDAR", 1),
            Err(IcsError::InvalidUtf8)
        );
    }

    #[test]
    fn incomplete_calendar_is_rejected() {
        assert_eq!(
            parse_ics(b"BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nEND:VEVENT", 1),
            Err(IcsError::NotCalendar)
        );
    }

    #[test]
    fn mismatched_calendar_end_is_rejected() {
        assert_eq!(
            parse_ics(b"BEGIN:VCALENDAR\nEND:VEVENT", 1),
            Err(IcsError::NotCalendar)
        );
    }

    #[test]
    fn component_names_are_case_insensitive() {
        let bytes = b"begin:vcalendar\nbegin:vevent\nuid:a\ndtstart:20261123t090000z\nend:vevent\nend:vcalendar";
        let row = &parse_ics(bytes, 1).unwrap().rows[0];
        assert_eq!(row.start_local, "2026-11-23T09:00:00");
        assert_eq!(row.tzid.as_deref(), Some("UTC"));
    }
}
