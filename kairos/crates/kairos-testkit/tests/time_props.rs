use jiff::{ToSpan, civil::DateTime, tz::AmbiguousOffset};
use kairos_core::time::zone::{utc_to_wall, wall_to_utc};
use proptest::prelude::*;

const ZONES: [&str; 5] = [
    "America/New_York",
    "Europe/Berlin",
    "Australia/Lord_Howe",
    "Asia/Kolkata",
    "UTC",
];

// Local start and duration in seconds of each spring-forward gap from 2025 to 2027.
const GAPS: [(&str, &str, i32); 9] = [
    ("America/New_York", "2025-03-09T02:00:00", 3600),
    ("America/New_York", "2026-03-08T02:00:00", 3600),
    ("America/New_York", "2027-03-14T02:00:00", 3600),
    ("Europe/Berlin", "2025-03-30T02:00:00", 3600),
    ("Europe/Berlin", "2026-03-29T02:00:00", 3600),
    ("Europe/Berlin", "2027-03-28T02:00:00", 3600),
    ("Australia/Lord_Howe", "2025-10-05T02:00:00", 1800),
    ("Australia/Lord_Howe", "2026-10-04T02:00:00", 1800),
    ("Australia/Lord_Howe", "2027-10-03T02:00:00", 1800),
];

proptest! {
    #[test]
    fn existing_wall_times_round_trip(
        zone in prop::sample::select(ZONES.to_vec()),
        seconds in 0..(1095 * 86_400_i32),
    ) {
        let start: DateTime = "2025-01-01T00:00:00".parse().unwrap();
        let wall = start.checked_add(seconds.seconds()).unwrap();
        let tz = jiff::tz::db().get(zone).unwrap();
        prop_assume!(!matches!(
            tz.to_ambiguous_timestamp(wall).offset(),
            AmbiguousOffset::Gap { .. }
        ));
        let utc = wall_to_utc(wall, zone).unwrap();
        prop_assert_eq!(utc_to_wall(utc, zone).unwrap(), wall);
    }

    #[test]
    fn skipped_wall_times_move_forward_by_the_gap(
        ((zone, start, duration), seconds) in prop::sample::select(GAPS.to_vec())
            .prop_flat_map(|gap| (Just(gap), 0..gap.2)),
    ) {
        let start: DateTime = start.parse().unwrap();
        let wall = start.checked_add(seconds.seconds()).unwrap();
        let expected = wall.checked_add(duration.seconds()).unwrap();
        let utc = wall_to_utc(wall, zone).unwrap();
        prop_assert_eq!(utc_to_wall(utc, zone).unwrap(), expected);
    }
}
