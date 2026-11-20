use std::{collections::BTreeMap, sync::LazyLock};

use kairos_types::api_v1::{EventPayload, ListEventsResponse, OccurrenceDto, PutEventResponse};
use matchit::Router;

use crate::{
    CoreError, Db, DbError, Hub, Ports,
    auth::tokens::verify_bearer,
    calendar::{
        events::{delete_series, event_from_row, upsert_event},
        model::{ExpandStatus, Origin, StoredEvent},
        reconcile::reconcile_series,
    },
    db::settings::get_settings,
    ics::expand::{ExpandOpts, expand_overlapping_all_day, expand_series},
    ports::Value,
    time::zone::utc_to_wall,
};

use super::{Method, Request, Response, validate};

#[derive(Clone, Copy)]
enum Route {
    Collection,
    Event,
}

static ROUTES: LazyLock<Result<Router<Route>, matchit::InsertError>> = LazyLock::new(|| {
    let mut router = Router::new();
    router.insert("/api/v1/events", Route::Collection)?;
    router.insert("/api/v1/events/{external_id}", Route::Event)?;
    Ok(router)
});

pub(super) async fn route<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let routes = ROUTES
        .as_ref()
        .map_err(|error| CoreError::Internal(error.to_string()))?;
    let route = routes.at(&req.path).map_err(|_| CoreError::NotFound)?;
    verify_bearer(hub, req)?;
    match (req.method, route.value) {
        (Method::Get, Route::Collection) => {
            let settings = get_settings(&hub.db)?;
            let (from, to) = validate::calendar_range(req, &settings.timezone)?;
            let (rows, truncated) =
                list_calendar_in_zone(&hub.db, &settings.timezone, false, |series| {
                    expand_series(series, from, to, &settings.timezone, &ExpandOpts::default())
                })?;
            Ok(Response::json(
                200,
                &ListEventsResponse {
                    occurrences: rows.into_iter().map(|row| row.occurrence).collect(),
                    truncated,
                },
            ))
        }
        (Method::Put | Method::Delete, Route::Event) => {
            let id =
                decode_external_id(route.params.get("external_id").ok_or(CoreError::NotFound)?)?;
            if req.method == Method::Put {
                put(hub, &id, req).await
            } else {
                delete(hub, &id).await
            }
        }
        _ => Err(CoreError::NotFound),
    }
}

fn decode_external_id(raw: &str) -> Result<String, CoreError> {
    let invalid = || validate::invalid("external_id", "invalid percent encoding");
    let mut parts = raw.split('%');
    let mut id = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        let (hex, tail) = part.split_at_checked(2).ok_or_else(invalid)?;
        let byte = u8::from_str_radix(hex, 16).map_err(|_| invalid())?;
        id.push(char::from(byte));
        id.push_str(tail);
    }
    validate::external_id(&id)?;
    Ok(id)
}

async fn put<P: Ports>(hub: &Hub<P>, id: &str, req: &Request) -> Result<Response, CoreError> {
    let payload: EventPayload = serde_json::from_slice(&req.body)
        .map_err(|error| validate::invalid("body", error.to_string()))?;
    let settings = get_settings(&hub.db)?;
    let event = validate::validate_payload(&payload, id, &settings.timezone)?;
    let now = hub.now();
    let saved = hub.db.transaction(|tx| {
        let saved = upsert_event(tx, &event, now)?;
        if saved.changed {
            reconcile_series(tx, &event.series_key, now)?;
        }
        let counts = tx.exec(
            "SELECT COUNT(*) AS n FROM reminders WHERE series_key=?1 AND status='pending'",
            &[Value::Text(event.series_key.clone())],
        )?;
        let count = counts
            .first()
            .ok_or_else(|| DbError("missing reminder count".into()))?
            .i64("n")?;
        let reminders_scheduled =
            u32::try_from(count).map_err(|error| DbError(error.to_string()))?;
        Ok(PutEventResponse {
            id: saved.id,
            created: saved.created,
            reminders_scheduled,
        })
    })?;
    hub.rearm_after_http().await;
    Ok(Response::json(200, &saved))
}

async fn delete<P: Ports>(hub: &Hub<P>, id: &str) -> Result<Response, CoreError> {
    let key = format!("api:{id}");
    let now = hub.now();
    hub.db.transaction(|tx| {
        if tx
            .exec(
                "SELECT 1 FROM events WHERE series_key=?1 AND origin='api'",
                &[Value::Text(key.clone())],
            )?
            .is_empty()
        {
            return Err(CoreError::NotFound);
        }
        delete_series(tx, &key, now)
    })?;
    hub.rearm_after_http().await;
    Ok(Response::empty(204))
}

pub struct CalendarOccurrence {
    pub occurrence: OccurrenceDto,
    pub event_id: i64,
    pub source_id: Option<i64>,
    pub color: Option<String>,
    pub series_key: String,
    pub slot: String,
}

pub fn list_occurrences<P: Ports>(
    hub: &Hub<P>,
    from: i64,
    to: i64,
    include_disabled: bool,
) -> Result<(Vec<OccurrenceDto>, Vec<String>), CoreError> {
    let (rows, truncated) = list_calendar(hub, from, to, include_disabled)?;
    Ok((
        rows.into_iter().map(|row| row.occurrence).collect(),
        truncated,
    ))
}

pub fn list_calendar<P: Ports>(
    hub: &Hub<P>,
    from: i64,
    to: i64,
    include_disabled: bool,
) -> Result<(Vec<CalendarOccurrence>, Vec<String>), CoreError> {
    let settings = get_settings(&hub.db)?;
    list_calendar_in_zone(&hub.db, &settings.timezone, include_disabled, |series| {
        expand_series(series, from, to, &settings.timezone, &ExpandOpts::default())
    })
}

pub(super) fn list_calendar_overlaps(
    db: &impl Db,
    from: i64,
    to: i64,
    zone: &str,
) -> Result<(Vec<CalendarOccurrence>, Vec<String>), CoreError> {
    list_calendar_in_zone(db, zone, false, |series| {
        expand_overlapping_all_day(series, from, to, zone)
    })
}

fn list_calendar_in_zone(
    db: &impl Db,
    zone: &str,
    include_disabled: bool,
    expand: impl Fn(&[StoredEvent]) -> crate::ics::expand::ExpandResult,
) -> Result<(Vec<CalendarOccurrence>, Vec<String>), CoreError> {
    let rows = db.exec(
        "SELECT e.*, s.name AS calendar_source_name, s.color AS calendar_source_color
         FROM events e LEFT JOIN sources s ON s.id=e.source_id
         WHERE ?1 OR e.source_id IS NULL OR s.enabled=1
         ORDER BY e.series_key,e.recurrence_id,e.id",
        &[Value::Integer(i64::from(include_disabled))],
    )?;
    let mut series = BTreeMap::<String, Vec<StoredEvent>>::new();
    let mut sources = BTreeMap::new();
    for row in rows {
        let event = event_from_row(&row)?;
        if let Some(id) = event.row.source_id {
            sources.entry(id).or_insert((
                row.opt_text("calendar_source_name")?,
                row.opt_text("calendar_source_color")?,
            ));
        }
        series
            .entry(event.row.series_key.clone())
            .or_default()
            .push(event);
    }
    let mut items = Vec::new();
    let mut truncated = Vec::new();
    for (key, series) in series {
        let expanded = expand(&series);
        if expanded.status != ExpandStatus::Complete {
            truncated.push(key);
        }
        let events = series
            .iter()
            .map(|event| (event.id, &event.row))
            .collect::<BTreeMap<_, _>>();
        for item in expanded.occurrences {
            let event = events
                .get(&item.event_id)
                .ok_or_else(|| DbError("missing expanded event".into()))?;
            let (source, color) = event
                .source_id
                .and_then(|id| sources.get(&id))
                .cloned()
                .unwrap_or_default();
            let origin = match event.origin {
                Origin::Api => "api",
                Origin::Ics => "ics",
                Origin::Manual => "manual",
            };
            let start_local = local_display(item.start_utc, zone, item.all_day)?;
            let end_local = local_display(item.end_utc, zone, item.all_day)?;
            items.push(CalendarOccurrence {
                occurrence: OccurrenceDto {
                    occ_key: item.occ_key,
                    origin: origin.into(),
                    external_id: event.external_id.clone(),
                    source,
                    title: event.title.clone(),
                    location: event.location.clone(),
                    all_day: item.all_day,
                    start_utc: item.start_utc,
                    end_utc: item.end_utc,
                    start_local,
                    end_local,
                },
                event_id: item.event_id,
                source_id: event.source_id,
                color,
                series_key: item.series_key,
                slot: item.slot,
            });
        }
    }
    items.sort_unstable_by(|a, b| {
        let a = &a.occurrence;
        let b = &b.occurrence;
        (a.start_utc, &a.occ_key).cmp(&(b.start_utc, &b.occ_key))
    });
    Ok((items, truncated))
}

fn local_display(at: i64, zone: &str, all_day: bool) -> Result<String, CoreError> {
    let wall = utc_to_wall(at, zone).map_err(|error| CoreError::Internal(error.to_string()))?;
    Ok(if all_day {
        wall.date().to_string()
    } else {
        format!("{wall:.0}")
    })
}
