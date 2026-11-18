//! Civil date and time conversion with RFC 5545 daylight-saving rules.

use std::fmt;

use jiff::{
    Timestamp,
    civil::{Date, DateTime},
    fmt::temporal::Pieces,
    tz::{self, TimeZone},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallValue {
    Timed(DateTime),
    Date(Date),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    BadFormat(String),
    UnknownZone(String),
    OutOfRange,
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadFormat(value) => write!(f, "invalid date or time {value:?}"),
            Self::UnknownZone(zone) => write!(f, "unknown time zone {zone:?}"),
            Self::OutOfRange => f.write_str("time is out of range"),
        }
    }
}

impl std::error::Error for TimeError {}

pub fn parse_wall(s: &str) -> Result<WallValue, TimeError> {
    let value = match s.len() {
        10 => s.parse::<Date>().ok().map(WallValue::Date),
        19 => s.parse::<DateTime>().ok().map(WallValue::Timed),
        _ => None,
    };
    // Jiff also accepts extended ISO formats. Stored wall values have one canonical spelling.
    value
        .filter(|value| format_wall(value) == s)
        .ok_or_else(|| TimeError::BadFormat(s.into()))
}

pub fn format_wall(v: &WallValue) -> String {
    match v {
        WallValue::Timed(wall) => format!("{wall:.0}"),
        WallValue::Date(date) => date.to_string(),
    }
}

pub fn is_valid_zone(name: &str) -> bool {
    get_zone(name).is_ok()
}

fn get_zone(name: &str) -> Result<TimeZone, TimeError> {
    // Jiff accepts this sentinel even though it has no IANA time-zone data.
    if name.eq_ignore_ascii_case("Etc/Unknown") {
        return Err(TimeError::UnknownZone(name.into()));
    }
    tz::db()
        .get(name)
        .map_err(|_| TimeError::UnknownZone(name.into()))
}

pub fn wall_to_utc(wall: DateTime, zone: &str) -> Result<i64, TimeError> {
    get_zone(zone)?
        .to_ambiguous_timestamp(wall)
        .compatible()
        .map(|instant| instant.as_millisecond())
        .map_err(|_| TimeError::OutOfRange)
}

pub fn utc_to_wall(ms: i64, zone: &str) -> Result<DateTime, TimeError> {
    let zone = get_zone(zone)?;
    let instant = Timestamp::from_millisecond(ms).map_err(|_| TimeError::OutOfRange)?;
    Ok(zone.to_datetime(instant))
}

pub fn parse_api_instant(s: &str, zone_hint: &str) -> Result<(String, String), TimeError> {
    let zone = get_zone(zone_hint)?;
    if parse_wall(s).is_ok() {
        return Ok((s.into(), zone_hint.into()));
    }

    let bad_format = || TimeError::BadFormat(s.into());
    if s.contains('[') {
        return Err(bad_format());
    }
    let pieces = Pieces::parse(s).map_err(|_| bad_format())?;
    let time = pieces.time().ok_or_else(bad_format)?;
    let offset = pieces.to_numeric_offset().ok_or_else(bad_format)?;
    let local = DateTime::from_parts(pieces.date(), time);
    let instant = offset
        .to_timestamp(local)
        .map_err(|_| TimeError::OutOfRange)?;
    let wall = WallValue::Timed(zone.to_datetime(instant));
    Ok((format_wall(&wall), zone_hint.into()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    use super::*;

    fn dt(value: &str) -> DateTime {
        value.parse().unwrap()
    }

    fn ms(value: &str) -> i64 {
        value.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn ny_gap_moves_forward() {
        assert_eq!(
            wall_to_utc(dt("2026-03-08T02:30:00"), "America/New_York").unwrap(),
            ms("2026-03-08T07:30:00Z"),
        );
    }

    #[test]
    fn berlin_gap_moves_forward() {
        assert_eq!(
            wall_to_utc(dt("2026-03-29T02:30:00"), "Europe/Berlin").unwrap(),
            ms("2026-03-29T01:30:00Z"),
        );
    }

    #[test]
    fn ny_overlap_takes_earlier() {
        assert_eq!(
            wall_to_utc(dt("2026-11-01T01:30:00"), "America/New_York").unwrap(),
            ms("2026-11-01T05:30:00Z"),
        );
    }

    #[test]
    fn berlin_overlap_takes_earlier() {
        assert_eq!(
            wall_to_utc(dt("2026-10-25T02:30:00"), "Europe/Berlin").unwrap(),
            ms("2026-10-25T00:30:00Z"),
        );
    }

    #[test]
    fn unknown_zone_is_error() {
        let invalid = "China Standard Time";
        let expected = TimeError::UnknownZone(invalid.into());
        assert_eq!(
            wall_to_utc(dt("2026-01-01T00:00:00"), invalid),
            Err(expected.clone()),
        );
        assert_eq!(utc_to_wall(0, invalid), Err(expected.clone()));
        assert_eq!(parse_api_instant("2026-01-01", invalid), Err(expected));
        assert!(!is_valid_zone(invalid));
        assert!(!is_valid_zone("Etc/Unknown"));
        assert!(is_valid_zone("UTC"));
        assert!(is_valid_zone("Australia/Lord_Howe"));
    }

    #[test]
    fn api_offset_becomes_wall_in_hint_zone() {
        assert_eq!(
            parse_api_instant("2026-11-20T09:00:00+08:00", "Europe/Berlin").unwrap(),
            ("2026-11-20T02:00:00".into(), "Europe/Berlin".into()),
        );
        assert_eq!(
            parse_api_instant("2026-11-20T23:00:00Z", "Asia/Kolkata").unwrap(),
            ("2026-11-21T04:30:00".into(), "Asia/Kolkata".into()),
        );
    }

    #[test]
    fn api_without_offset_keeps_wall() {
        for input in ["2026-11-20T09:00:00", "2026-11-20"] {
            assert_eq!(
                parse_api_instant(input, "Asia/Tokyo").unwrap(),
                (input.into(), "Asia/Tokyo".into()),
            );
        }
    }

    #[test]
    fn wall_values_use_canonical_shapes() {
        let all_day = "2024-02-29";
        let timed = "2024-02-29T00:00:00";
        assert!(matches!(parse_wall(all_day), Ok(WallValue::Date(_))));
        assert!(matches!(parse_wall(timed), Ok(WallValue::Timed(_))));
        for input in [all_day, timed] {
            assert_eq!(format_wall(&parse_wall(input).unwrap()), input);
        }
        for input in [
            "2026-02-29",
            "2024-02-29 00:00:00",
            "2024-02-29T00:00",
            "2024-02-29T00:00:00Z",
            "2024-02-29T00:00:00+05:00",
        ] {
            assert_eq!(parse_wall(input), Err(TimeError::BadFormat(input.into())));
        }
    }

    #[test]
    fn conversion_rejects_out_of_range_values() {
        assert_eq!(utc_to_wall(i64::MAX, "UTC"), Err(TimeError::OutOfRange));
        assert_eq!(
            wall_to_utc(DateTime::MIN, "UTC"),
            Err(TimeError::OutOfRange)
        );
        assert_eq!(
            parse_api_instant("9999-12-31T23:59:59Z", "UTC"),
            Err(TimeError::OutOfRange),
        );
    }
}
