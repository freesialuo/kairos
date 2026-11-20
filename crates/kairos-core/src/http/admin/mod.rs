mod channel;
mod events;
mod export;
mod settings;
mod sources;

use crate::{
    CoreError, Db, Hub, Ports,
    auth::{
        csrf::check_origin,
        session::require_full_session,
        tokens::{create_token, list_tokens, revoke_token},
    },
    db::settings::get_meta,
    http::{Method, Request, Response, api_v1},
    ports::Value as V,
    sched::queue::queue_length,
};
use serde::Serialize;
use serde_json::{Value as Json, json};

fn invalid(field: &'static str) -> CoreError {
    CoreError::Invalid {
        field,
        message: format!("invalid {field}"),
    }
}
fn id(value: &str) -> Result<i64, CoreError> {
    value
        .parse()
        .ok()
        .filter(|&n| n > 0)
        .ok_or(CoreError::NotFound)
}
fn body<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, CoreError> {
    serde_json::from_slice(&req.body).map_err(|_| invalid("body"))
}
fn ok<T: Serialize>(data: T) -> Result<Response, CoreError> {
    Ok(Response::json(200, &data))
}
fn query<'a>(req: &'a Request, key: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}
fn valid_url(value: &str, allow_http: bool) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.has_host()
            && (url.scheme() == "https" || (allow_http && url.scheme() == "http"))
            && url.username().is_empty()
            && url.password().is_none()
    })
}
fn rows(db: &impl Db, sql: &str, args: &[V]) -> Result<Vec<Json>, CoreError> {
    Ok(db
        .exec(sql, args)?
        .into_iter()
        .map(|r| r.into_json())
        .collect())
}

pub async fn route<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let session = require_full_session(hub, req)?;
    check_origin(req, &hub.config.origin)?;
    let suffix = req
        .path
        .strip_prefix("/api/admin/")
        .ok_or(CoreError::NotFound)?;
    let path: Vec<_> = suffix.split('/').collect();
    let mut response = match path.as_slice() {
        ["settings"] => settings::handle(hub, req),
        ["queue"] if req.method == Method::Get => ok(json!({"length":queue_length(&hub.db)?})),
        ["sources", rest @ ..] => sources::route(hub, req, rest).await,
        ["events", rest @ ..] => events::route(hub, req, rest),
        ["channel", rest @ ..] => channel::route(hub, req, rest).await,
        ["calendar"] if req.method == Method::Get => calendar(hub, req),
        ["passkeys", rest @ ..] => passkeys(hub, req, rest),
        ["tokens", rest @ ..] => tokens(hub, req, rest),
        ["log"] if req.method == Method::Get => log(hub),
        ["export.json"] if req.method == Method::Get => export::json_export(&hub.db),
        ["export.ics"] if req.method == Method::Get => export::ics(&hub.db, hub.now()),
        _ => Err(CoreError::NotFound),
    }?;
    if req.method != Method::Get {
        hub.rearm_after_http().await;
    }
    response
        .headers
        .extend(crate::auth::session::session_cookie_for_request(
            req,
            &session,
            hub.now(),
        ));
    Ok(response)
}

fn calendar<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let timezone = crate::db::settings::get_settings(&hub.db)?.timezone;
    let (from, to) = crate::http::validate::calendar_range(req, &timezone)?;
    let (occs, truncated) = api_v1::list_calendar_overlaps(&hub.db, from, to, &timezone)?;
    let earlier_starts: Vec<_> = occs
        .iter()
        .map(|entry| entry.occurrence.start_utc)
        .filter(|&start| start < from)
        .collect();
    let reminders = hub.db.exec(
        "SELECT key,fire_at,status FROM reminders WHERE occ_start_utc<?2
          AND (occ_start_utc>=?1 OR occ_start_utc IN (SELECT value FROM json_each(?3)))",
        &[
            V::Integer(from),
            V::Integer(to),
            V::Text(json!(earlier_starts).to_string()),
        ],
    )?;
    let mut groups = std::collections::HashMap::<String, Vec<Json>>::new();
    for row in reminders {
        let key = row.text("key")?;
        if let Some((occ, _)) = key.rsplit_once('|') {
            groups
                .entry(occ.to_string())
                .or_default()
                .push(row.into_json());
        }
    }
    let events = occs
        .into_iter()
        .map(|c| {
            #[derive(Serialize)]
            struct CalendarEntry {
                event_id: i64,
                color: Option<String>,
                slot: String,
                deletable: bool,
                editable: bool,
                reminders: Vec<Json>,
                #[serde(flatten)]
                occurrence: kairos_types::api_v1::OccurrenceDto,
            }
            CalendarEntry {
                event_id: c.event_id,
                color: c.color,
                slot: c.slot,
                deletable: c.occurrence.origin != "ics",
                editable: c.occurrence.origin == "manual",
                reminders: groups.remove(&c.occurrence.occ_key).unwrap_or_default(),
                occurrence: c.occurrence,
            }
        })
        .collect::<Vec<_>>();
    ok(json!({"occurrences":events,"truncated":truncated}))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Name {
    name: String,
}
fn name(req: &Request) -> Result<String, CoreError> {
    let name = body::<Name>(req)?.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid("name"));
    }
    Ok(name)
}
fn tokens<P: Ports>(hub: &Hub<P>, req: &Request, parts: &[&str]) -> Result<Response, CoreError> {
    match (req.method, parts) {
        (Method::Get, []) => ok(list_tokens(&hub.db)?),
        (Method::Post, []) => {
            let (id, token) = create_token(hub, &name(req)?)?;
            ok(json!({"id":id,"token":token}))
        }
        (Method::Delete, [key]) => {
            revoke_token(&hub.db, id(key)?)?;
            Ok(Response::empty(204))
        }
        _ => Err(CoreError::NotFound),
    }
}

fn passkeys<P: Ports>(hub: &Hub<P>, req: &Request, parts: &[&str]) -> Result<Response, CoreError> {
    let rp = &hub.config.rp_id;
    match(req.method,parts){
        (Method::Get,[])=>{
            let mut keys=rows(&hub.db,"SELECT id,name,rp_id,created_at,last_used_at FROM passkeys ORDER BY created_at,id",&[])?;
            for key in &mut keys{if let Some(data)=key.as_object_mut(){let stale=data.get("rp_id").and_then(Json::as_str)!=Some(rp);data.insert("stale".into(),json!(stale));}}ok(keys)
        }
        (Method::Patch,[key])=>{
            if hub.db.exec("UPDATE passkeys SET name=?2 WHERE id=?1 RETURNING id",&[V::Text((*key).into()),V::Text(name(req)?)] )?.is_empty(){return Err(CoreError::NotFound);}
            Ok(Response::empty(204))
        }
        (Method::Delete,[key])=>hub.db.transaction(|db|{
            let info=db.exec("SELECT rp_id,(SELECT count(*) FROM passkeys WHERE rp_id=?2) AS current_keys FROM passkeys WHERE id=?1",
                &[V::Text((*key).into()),V::Text(rp.clone())])?;
            let row=info.first().ok_or(CoreError::NotFound)?;
            if row.text("rp_id")?==*rp && row.i64("current_keys")?==1 && query(req,"confirm")!=Some("1"){
                return Err(CoreError::Conflict{code:"confirm_required",message:format!("This is your last passkey for {rp}. Without it you can only sign in with the recovery code.")});
            }
            db.exec("DELETE FROM passkeys WHERE id=?1",&[V::Text((*key).into())])?;Ok(Response::empty(204))
        }),
        _=>Err(CoreError::NotFound)
    }
}
fn log<P: Ports>(hub: &Hub<P>) -> Result<Response, CoreError> {
    let db = &hub.db;
    let mut background = serde_json::Map::new();
    for key in [
        "last_auth_error",
        "alarm_error_streak",
        "last_alarm_error",
        "last_maintenance_at",
        "last_maintenance_error",
        "last_watchdog_at",
    ] {
        background.insert(key.into(), json!(get_meta(db, key)));
    }
    ok(json!({
        "failed_reminders":rows(db,"SELECT key,series_key,fire_at,last_error,attempts FROM reminders WHERE status='failed' ORDER BY fire_at DESC",&[])?,
        "deliveries":rows(db,"SELECT id,at,reminder_key,kind,channel,ok,error,summary FROM deliveries ORDER BY at DESC,id DESC LIMIT 200",&[])?,
        "failing_sources":rows(db,"SELECT id,name,last_error,consecutive_failures FROM sources WHERE consecutive_failures>0",&[])?,
        "expansion_warnings":rows(db,"SELECT id,title,series_key,expand_status,expand_warning FROM events WHERE expand_warning IS NOT NULL",&[])?,
        "background":background
    }))
}
