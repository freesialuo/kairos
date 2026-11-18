use super::*;
use crate::{
    calendar::{events::load_series, model::StoredEvent},
    db::settings::get_settings,
    sources::model::list_sources,
};

pub(super) fn events(db: &impl Db, include_disabled: bool) -> Result<Vec<StoredEvent>, CoreError> {
    let keys = db.exec(
        "SELECT DISTINCT e.series_key FROM events e LEFT JOIN sources s ON e.source_id=s.id
        WHERE ?1 OR s.enabled IS NULL OR s.enabled=1 ORDER BY e.series_key",
        &[V::Integer(i64::from(include_disabled))],
    )?;
    let mut events = Vec::new();
    for key in keys {
        events.extend(load_series(db, &key.text("series_key")?)?);
    }
    Ok(events)
}

pub(super) fn json_export(db: &impl Db) -> Result<Response, CoreError> {
    let rows = events(db, true)?
        .into_iter()
        .map(|e| json!({"id":e.id,"event":e.row}))
        .collect::<Vec<_>>();
    let mut response = ok(
        json!({"events":rows,"sources":list_sources(db)?,"settings":get_settings(db)?,"channel":super::channel::view(db)?}),
    )?;
    response.headers.push((
        "Content-Disposition".into(),
        "attachment; filename=kairos.json".into(),
    ));
    Ok(response)
}

fn text_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace("\r\n", "\n")
        .replace(['\r', '\n'], "\\n")
        .replace(';', "\\;")
        .replace(',', "\\,")
}
fn line(out: &mut String, s: &str) {
    let mut width = 0;
    for ch in s.chars() {
        // Property values cannot create a second content line.
        if matches!(ch, '\r' | '\n') {
            continue;
        }
        if width + ch.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += ch.len_utf8();
    }
    out.push_str("\r\n");
}
fn date_field(prop: &str, wall: &str, zone: Option<&str>, all_day: bool) -> String {
    let basic = wall.replace(['-', ':'], "");
    if all_day {
        format!("{prop};VALUE=DATE:{basic}")
    } else {
        match zone {
            Some("UTC") => format!("{prop}:{basic}Z"),
            Some(z) => format!("{prop};TZID={z}:{basic}"),
            None => format!("{prop}:{basic}"),
        }
    }
}

pub(super) fn ics(db: &impl Db, now: i64) -> Result<Response, CoreError> {
    let mut out =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Kairos//Calendar//EN\r\n");
    let stamp = jiff::Timestamp::from_millisecond(now)
        .map_err(|e| CoreError::Internal(e.to_string()))?
        .strftime("%Y%m%dT%H%M%SZ")
        .to_string();
    let rows = events(db, false)?;
    let parents: std::collections::HashMap<_, _> = rows
        .iter()
        .filter(|e| e.row.recurrence_id.is_none())
        .map(|e| (&e.row.series_key, &e.row))
        .collect();
    for e in &rows {
        let r = &e.row;
        line(&mut out, "BEGIN:VEVENT");
        line(&mut out, &format!("UID:{}", text_value(&r.series_key)));
        line(&mut out, &format!("DTSTAMP:{stamp}"));
        line(&mut out, &format!("SUMMARY:{}", text_value(&r.title)));
        if let Some(v) = &r.location {
            line(&mut out, &format!("LOCATION:{}", text_value(v)));
        }
        if let Some(v) = &r.description {
            line(&mut out, &format!("DESCRIPTION:{}", text_value(v)));
        }
        line(
            &mut out,
            &date_field("DTSTART", &r.start_local, r.tzid.as_deref(), r.all_day),
        );
        if r.duration_sec > 0 {
            let duration = if r.all_day {
                format!("P{}D", r.duration_sec / 86400)
            } else {
                format!("PT{}S", r.duration_sec)
            };
            line(&mut out, &format!("DURATION:{duration}"));
        }
        if let Some(slot) = &r.recurrence_id {
            let parent = parents.get(&r.series_key).copied().unwrap_or(r);
            line(
                &mut out,
                &date_field(
                    "RECURRENCE-ID",
                    slot,
                    parent.tzid.as_deref(),
                    parent.all_day,
                ),
            );
        }
        if let Some(rule) = &r.rrule {
            line(&mut out, &format!("RRULE:{rule}"));
        }
        for slot in &r.exdates {
            line(
                &mut out,
                &date_field("EXDATE", slot, r.tzid.as_deref(), r.all_day),
            );
        }
        line(&mut out, "END:VEVENT");
    }
    line(&mut out, "END:VCALENDAR");
    Ok(Response {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "text/calendar; charset=utf-8".into()),
            (
                "Content-Disposition".into(),
                "attachment; filename=kairos.ics".into(),
            ),
        ],
        body: out.into_bytes(),
    })
}
