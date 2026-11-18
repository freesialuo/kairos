use super::*;
use crate::{
    calendar::{
        events::{add_exdate, delete_series, insert_manual_event, load_series, upsert_event},
        model::Origin,
        reconcile::reconcile_series,
    },
    db::settings::get_settings,
    http::validate::validate_payload,
    time::zone::{WallValue, parse_wall},
};
use kairos_types::{
    admin::{ManualEventInput, ReminderChoice},
    api_v1::EventPayload,
    repeat::to_rrule,
};

pub(super) fn route<P: Ports>(
    hub: &Hub<P>,
    req: &Request,
    parts: &[&str],
) -> Result<Response, CoreError> {
    if parts.is_empty() && req.method == Method::Get {
        let timezone = get_settings(&hub.db)?.timezone;
        return ok(super::export::events(&hub.db, true)?
            .into_iter()
            .filter(|e| e.row.origin == Origin::Manual)
            .map(|e| {
                Ok::<_, CoreError>(
                    json!({"id":e.id,"end_local":end_local(&e.row,&timezone)?,"event":e.row}),
                )
            })
            .collect::<Result<Vec<_>, _>>()?);
    }
    let existing = if let Some(raw) = parts.first() {
        let rows = hub.db.exec(
            "SELECT series_key FROM events WHERE id=?1",
            &[V::Integer(id(raw)?)],
        )?;
        let key = rows
            .first()
            .ok_or(CoreError::NotFound)?
            .text("series_key")?;
        Some(
            load_series(&hub.db, &key)?
                .into_iter()
                .find(|r| r.id == id(raw).unwrap_or(-1))
                .ok_or(CoreError::NotFound)?,
        )
    } else {
        None
    };
    match (req.method, parts, existing) {
        (Method::Delete, [_], Some(event)) if event.row.origin != Origin::Ics => {
            hub.db
                .transaction(|db| delete_series(db, &event.row.series_key, hub.now()))?;
            Ok(Response::empty(204))
        }
        (Method::Post, [_, "skip"], Some(event)) => {
            if event.row.origin != Origin::Manual || event.row.rrule.is_none() {
                return Err(not_manual());
            }
            #[derive(serde::Deserialize)]
            struct Skip {
                slot: String,
            }
            let slot = body::<Skip>(req)?.slot;
            if !parse_wall(&slot)
                .is_ok_and(|wall| matches!(wall, WallValue::Date(_)) == event.row.all_day)
            {
                return Err(invalid("slot"));
            }
            if event.row.exdates.len() >= 200 && !event.row.exdates.contains(&slot) {
                return Err(invalid("exdates"));
            }
            hub.db.transaction(|db| {
                add_exdate(db, event.id, &slot, hub.now())?;
                reconcile_series(db, &event.row.series_key, hub.now())?;
                Ok(())
            })?;
            Ok(Response::empty(204))
        }
        (Method::Post, [], None) | (Method::Patch, [_], Some(_)) => {
            // On PATCH the form sends the full edited series.
            let item = if let [raw] = parts {
                Some(id(raw)?)
            } else {
                None
            };
            let input: ManualEventInput = body(req)?;
            let rule = input
                .repeat
                .as_ref()
                .map(|p| to_rrule(p, &input.start))
                .transpose()
                .map_err(|_| invalid("repeat"))?;
            let payload = EventPayload {
                title: input.title,
                start: input.start,
                end: input.end,
                all_day: Some(input.all_day),
                timezone: input.timezone,
                location: input.location,
                description: input.description,
                rrule: rule,
                exdates: None,
                reminders: match input.reminders {
                    ReminderChoice::Default => None,
                    ReminderChoice::Custom(leads) => Some(leads),
                    ReminderChoice::None => Some(vec![]),
                },
            };
            let mut row = validate_payload(&payload, "manual", &get_settings(&hub.db)?.timezone)?;
            row.origin = Origin::Manual;
            row.external_id = None;
            let event_id = hub.db.transaction(|db| {
                let id = if let Some(id) = item {
                    let records = db.exec(
                        "SELECT series_key,origin FROM events WHERE id=?1",
                        &[V::Integer(id)],
                    )?;
                    let old = records.first().ok_or(CoreError::NotFound)?;
                    if old.text("origin")? != "manual" {
                        return Err(not_manual());
                    }
                    row.series_key = old.text("series_key")?;
                    if row.rrule.is_some()
                        && let Some(parent) = load_series(db, &row.series_key)?.first()
                    {
                        row.exdates.clone_from(&parent.row.exdates);
                    }
                    upsert_event(db, &row, hub.now())?;
                    id
                } else {
                    insert_manual_event(db, row, hub.now())?
                };
                reconcile_series(db, &format!("manual:{id}"), hub.now())?;
                Ok(id)
            })?;
            ok(json!({"id":event_id}))
        }
        (Method::Delete, [_], _) => Err(not_manual()),
        _ => Err(CoreError::NotFound),
    }
}

fn not_manual() -> CoreError {
    CoreError::Conflict {
        code: "not_manual",
        message: "This operation requires a manual event.".into(),
    }
}

fn end_local(row: &crate::calendar::model::EventRow, user_zone: &str) -> Result<String, CoreError> {
    use crate::time::zone::{format_wall, utc_to_wall, wall_to_utc};
    let zone = row.tzid.as_deref().unwrap_or(user_zone);
    let err = |e: crate::time::zone::TimeError| CoreError::Internal(e.to_string());
    let wall = match parse_wall(&row.start_local).map_err(err)? {
        WallValue::Timed(w) => {
            let start = match row.initial_start_utc {
                Some(ms) => ms,
                None => wall_to_utc(w, zone).map_err(err)?,
            };
            WallValue::Timed(
                utc_to_wall(
                    start.saturating_add(row.duration_sec.saturating_mul(1000)),
                    zone,
                )
                .map_err(err)?,
            )
        }
        WallValue::Date(date) => {
            let span = jiff::Span::new()
                .try_days(row.duration_sec / 86_400)
                .map_err(|e| CoreError::Internal(e.to_string()))?;
            WallValue::Date(
                date.checked_add(span)
                    .map_err(|e| CoreError::Internal(e.to_string()))?,
            )
        }
    };
    Ok(format_wall(&wall))
}
