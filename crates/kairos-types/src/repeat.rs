use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    Daily,
    Weekly { days: Vec<String> },
    Monthly,
    Yearly,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum End {
    Never,
    Until(String),
    Count(u16),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepeatPreset {
    pub freq: Frequency,
    pub interval: u8,
    pub end: End,
}

pub fn to_rrule(p: &RepeatPreset, start: &str) -> Result<String, &'static str> {
    if !(1..=99).contains(&p.interval) {
        return Err("invalid interval");
    }
    let mut rule = match &p.freq {
        Frequency::Daily => "FREQ=DAILY".into(),
        Frequency::Weekly { days } => {
            if days.is_empty()
                || days.len() > 7
                || days
                    .iter()
                    .any(|s| !["MO", "TU", "WE", "TH", "FR", "SA", "SU"].contains(&s.as_str()))
            {
                return Err("invalid weekdays");
            }
            format!("FREQ=WEEKLY;BYDAY={}", days.join(","))
        }
        Frequency::Monthly => {
            let day = start
                .get(8..10)
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|d| (1..=31).contains(d))
                .ok_or("invalid start date")?;
            format!("FREQ=MONTHLY;BYMONTHDAY={day}")
        }
        Frequency::Yearly => "FREQ=YEARLY".into(),
    };
    if p.interval != 1 {
        rule.push_str(&format!(";INTERVAL={}", p.interval));
    }
    match &p.end {
        End::Never => (),
        End::Count(n) if (1..=999).contains(n) => rule.push_str(&format!(";COUNT={n}")),
        End::Until(date) => {
            if date.len() != 10
                || date.get(4..5) != Some("-")
                || date.get(7..8) != Some("-")
                || date.replace('-', "").len() != 8
                || !date.replace('-', "").bytes().all(|b| b.is_ascii_digit())
            {
                return Err("invalid ending date");
            }
            rule.push_str(&format!(
                ";UNTIL={}{}",
                date.replace('-', ""),
                if start.len() > 10 { "T235959" } else { "" }
            ));
        }
        _ => return Err("invalid count"),
    }
    Ok(rule)
}

pub fn from_rrule(rule: &str, start: &str) -> Option<RepeatPreset> {
    let fields = rule
        .split(';')
        .map(|s| s.split_once('='))
        .collect::<Option<std::collections::BTreeMap<_, _>>>()?;
    let freq = match *fields.get("FREQ")? {
        "DAILY" => Frequency::Daily,
        "YEARLY" => Frequency::Yearly,
        "MONTHLY" => Frequency::Monthly,
        "WEEKLY" => Frequency::Weekly {
            days: fields.get("BYDAY")?.split(',').map(str::to_owned).collect(),
        },
        _ => return None,
    };
    let interval = fields
        .get("INTERVAL")
        .map(|v| v.parse::<u8>())
        .transpose()
        .ok()?
        .unwrap_or(1);
    let end = if let Some(n) = fields.get("COUNT") {
        End::Count(n.parse().ok()?)
    } else if let Some(v) = fields.get("UNTIL") {
        End::Until(format!("{}-{}-{}", v.get(..4)?, v.get(4..6)?, v.get(6..8)?))
    } else {
        End::Never
    };
    let preset = RepeatPreset {
        freq,
        interval,
        end,
    };
    // Only offer a preset when saving it can reproduce all of the original constraints.
    let written = to_rrule(&preset, start).ok()?;
    let mut expected: Vec<_> = written.split(';').collect();
    let mut actual: Vec<_> = rule.split(';').filter(|f| *f != "INTERVAL=1").collect();
    expected.sort_unstable();
    actual.sort_unstable();
    (expected == actual).then_some(preset)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_manual_presets() {
        for freq in [
            Frequency::Daily,
            Frequency::Weekly {
                days: vec!["MO".into(), "FR".into()],
            },
            Frequency::Monthly,
            Frequency::Yearly,
        ] {
            for end in [End::Never, End::Count(8), End::Until("2027-02-28".into())] {
                let preset = RepeatPreset {
                    freq: freq.clone(),
                    interval: 2,
                    end,
                };
                let start = "2026-11-23T12:00:00";
                let rule = to_rrule(&preset, start).unwrap();
                assert_eq!(from_rrule(&rule, start), Some(preset));
            }
        }
    }
}
