use jiff::{Timestamp, civil::Date, tz::Offset};
use kairos_types::{api_v1::EventPayload, limits::*};

use crate::{
    CoreError,
    calendar::model::{EventRow, Origin},
    http::Request,
    ics::expand::validate_rrule,
    time::zone::{
        WallValue, format_wall, is_valid_zone, parse_api_instant, parse_wall, wall_to_utc,
    },
};

const DAY_SECONDS: i64 = 86_400;

pub fn invalid(field: &'static str, message: impl Into<String>) -> CoreError {
    CoreError::Invalid {
        field,
        message: message.into(),
    }
}

pub fn external_id(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > MAX_EXTERNAL_ID_CHARS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(invalid(
            "external_id",
            "expected 1–200 ASCII letters, digits or ._:-",
        ));
    }
    Ok(())
}

pub fn validate_payload(p: &EventPayload, id: &str, user_tz: &str) -> Result<EventRow, CoreError> {
    external_id(id)?;
    if p.title.is_empty() || p.title.chars().count() > MAX_TITLE_CHARS {
        return Err(invalid("title", "expected 1–200 characters"));
    }
    if p.location
        .as_ref()
        .is_some_and(|v| v.chars().count() > MAX_LOCATION_CHARS)
    {
        return Err(invalid("location", "must contain at most 2,000 characters"));
    }
    if p.description
        .as_ref()
        .is_some_and(|v| v.chars().count() > MAX_DESCRIPTION_CHARS)
    {
        return Err(invalid(
            "description",
            "must contain at most 2,000 characters",
        ));
    }
    if p.reminders
        .as_ref()
        .is_some_and(|v| v.len() > MAX_REMINDERS || v.iter().any(|m| *m > MAX_REMINDER_MINUTES))
    {
        return Err(invalid(
            "reminders",
            "expected at most five leads of 0–10,080 minutes",
        ));
    }
    if let Some(rule) = &p.rrule {
        validate_rrule(rule).map_err(|error| invalid("rrule", error))?;
    }
    let zone = p.timezone.as_deref().unwrap_or(user_tz);
    if !is_valid_zone(zone) {
        return Err(invalid("timezone", "expected an IANA time zone"));
    }
    let start = parse_time(&p.start, zone, "start")?;
    let date_only = matches!(start.wall, WallValue::Date(_));
    let all_day = p.all_day.unwrap_or(date_only);
    if all_day != date_only {
        return Err(invalid(
            "start",
            "all-day events require a date and timed events require a date-time",
        ));
    }
    let end = p
        .end
        .as_deref()
        .map(|s| parse_time(s, zone, "end"))
        .transpose()?;
    let duration_sec = match (start.wall, end) {
        (
            WallValue::Date(start),
            Some(ParsedTime {
                wall: WallValue::Date(end),
                ..
            }),
        ) if end > start => {
            whole_days_seconds(start, end).map_err(|_| invalid("end", "date is out of range"))?
        }
        (
            WallValue::Timed(_),
            Some(ParsedTime {
                wall: WallValue::Timed(_),
                at,
                ..
            }),
        ) if at >= start.at => (at - start.at) / 1_000,
        (WallValue::Date(_), None) => DAY_SECONDS,
        (WallValue::Timed(_), None) => 0,
        _ => {
            return Err(invalid(
                "end",
                "end precedes start or does not match its date/time form",
            ));
        }
    };
    let exdates = parse_exdates(p, zone, date_only)?;
    let tzid = (!all_day && (p.timezone.is_some() || start.explicit)).then(|| zone.to_owned());
    // A later overlap instant cannot be recovered by resolving its local time alone.
    let initial_start_utc = (start.at != start.compatible).then_some(start.at);
    Ok(EventRow {
        origin: Origin::Api,
        source_id: None,
        series_key: format!("api:{id}"),
        uid: None,
        recurrence_id: None,
        external_id: Some(id.into()),
        title: p.title.clone(),
        description: p.description.clone(),
        location: p.location.clone(),
        all_day,
        start_local: format_wall(&start.wall),
        initial_start_utc,
        tzid,
        duration_sec,
        rrule: p.rrule.clone(),
        exdates,
        lead_minutes: p.reminders.clone(),
        valarm_minutes: Vec::new(),
    })
}

fn parse_exdates(p: &EventPayload, zone: &str, date_only: bool) -> Result<Vec<String>, CoreError> {
    let Some(values) = p.exdates.as_ref() else {
        return Ok(Vec::new());
    };
    if p.rrule.is_none() || values.len() > MAX_EXDATES {
        return Err(invalid(
            "exdates",
            "expected a recurrence rule and at most 200 excluded dates",
        ));
    }
    values
        .iter()
        .map(|value| {
            let parsed = parse_time(value, zone, "exdates")?;
            if matches!(parsed.wall, WallValue::Date(_)) != date_only {
                return Err(invalid(
                    "exdates",
                    "excluded dates must use the same form as start",
                ));
            }
            Ok(format_wall(&parsed.wall))
        })
        .collect()
}

pub fn calendar_range(req: &Request, user_tz: &str) -> Result<(i64, i64), CoreError> {
    let query = |key: &'static str| {
        req.query
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
            .ok_or_else(|| invalid(key, "missing range boundary"))
    };
    let from = parse_time(query("from")?, user_tz, "from")?;
    let to = parse_time(query("to")?, user_tz, "to")?;
    let span = match (from.wall, to.wall) {
        (WallValue::Date(start), WallValue::Date(end)) => {
            whole_days_seconds(start, end).map_err(|_| invalid("to", "date is out of range"))?
                * 1_000
        }
        _ => to.at - from.at,
    };
    if from.at >= to.at || span > MAX_RANGE_DAYS * DAY_SECONDS * 1_000 {
        return Err(invalid("to", "range must be positive and at most 92 days"));
    }
    Ok((from.at, to.at))
}

struct ParsedTime {
    wall: WallValue,
    at: i64,
    compatible: i64,
    explicit: bool,
}

fn parse_time(input: &str, zone: &str, field: &'static str) -> Result<ParsedTime, CoreError> {
    let parse_error = |error: crate::time::zone::TimeError| invalid(field, error.to_string());
    let (wall, at) = match parse_wall(input) {
        Ok(wall) => (wall, None),
        Err(_) => {
            let (canonical, _) = parse_api_instant(input, zone).map_err(parse_error)?;
            let wall = parse_wall(&canonical).map_err(parse_error)?;
            let at = input
                .parse::<Timestamp>()
                .map_err(|error| invalid(field, error.to_string()))?;
            (wall, Some(at.as_millisecond()))
        }
    };
    let wall_time = match wall {
        WallValue::Timed(time) => time,
        WallValue::Date(date) => date.at(0, 0, 0, 0),
    };
    let compatible = wall_to_utc(wall_time, zone).map_err(parse_error)?;
    Ok(ParsedTime {
        wall,
        at: at.unwrap_or(compatible),
        compatible,
        explicit: at.is_some(),
    })
}

fn whole_days_seconds(start: Date, end: Date) -> Result<i64, jiff::Error> {
    let start = Offset::UTC.to_timestamp(start.at(0, 0, 0, 0))?;
    let end = Offset::UTC.to_timestamp(end.at(0, 0, 0, 0))?;
    Ok(end.as_second() - start.as_second())
}
