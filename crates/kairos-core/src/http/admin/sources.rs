use super::*;
use crate::{
    sched::queue::{SeriesFilter, enqueue_where},
    sources::{
        model::{SourceRow, get_source, list_sources},
        sync::sync_source,
    },
};
use kairos_types::admin::SourceInput;

pub(super) async fn route<P: Ports>(
    hub: &Hub<P>,
    req: &Request,
    parts: &[&str],
) -> Result<Response, CoreError> {
    match (req.method, parts) {
        (Method::Get, []) => ok(list_sources(&hub.db)?),
        (Method::Post, []) | (Method::Patch, [_]) => {
            let input: SourceInput = body(req)?;
            let source = if let [key] = parts {
                get_source(&hub.db, id(key)?)?
                    .ok_or(CoreError::NotFound)
                    .map(Some)?
            } else {
                None
            };
            save(hub, input, source)
        }
        (Method::Delete, [key]) => {
            let key = id(key)?;
            let queued = hub.db.transaction(|db| {
                let queued =
                    enqueue_where(db, SeriesFilter::Source(key), "source_deleted", hub.now())?;
                db.exec("DELETE FROM events WHERE source_id=?1", &[V::Integer(key)])?;
                if db
                    .exec(
                        "DELETE FROM sources WHERE id=?1 RETURNING id",
                        &[V::Integer(key)],
                    )?
                    .is_empty()
                {
                    return Err(CoreError::NotFound);
                }
                Ok(queued)
            })?;
            ok(json!({"queued":queued}))
        }
        (Method::Post, [key, "sync"]) => {
            let key = id(key)?;
            let source = get_source(&hub.db, key)?.ok_or(CoreError::NotFound)?;
            if !source.enabled {
                return Err(invalid("enabled"));
            }
            // The resulting source includes the failure text on unsuccessful refreshes.
            let _ = sync_source(hub, &source, hub.now()).await;
            ok(get_source(&hub.db, key)?.ok_or(CoreError::NotFound)?)
        }
        _ => Err(CoreError::NotFound),
    }
}

fn save<P: Ports>(
    hub: &Hub<P>,
    input: SourceInput,
    source: Option<SourceRow>,
) -> Result<Response, CoreError> {
    let old = source.as_ref();
    let name = input
        .name
        .or_else(|| old.map(|s| s.name.clone()))
        .ok_or_else(|| invalid("name"))?;
    let url = input
        .url
        .or_else(|| old.map(|s| s.url.clone()))
        .ok_or_else(|| invalid("url"))?;
    let color = input
        .color
        .or_else(|| old.map(|s| s.color.clone()))
        .unwrap_or_else(|| "#45786b".into());
    let enabled = input.enabled.unwrap_or(old.is_none_or(|s| s.enabled));
    let notify = input.notify.unwrap_or(old.is_none_or(|s| s.notify));
    let honor = input
        .honor_valarm
        .unwrap_or(old.is_none_or(|s| s.honor_valarm));
    let interval = input
        .interval_min
        .unwrap_or(old.map_or(30, |s| s.interval_min));
    let lead = input
        .default_lead_min
        .unwrap_or(old.and_then(|s| s.default_lead_min));
    if name.trim().is_empty() || name.chars().count() > 200 {
        return Err(invalid("name"));
    }
    if !valid_url(&url, hub.config.allow_http_urls) {
        return Err(invalid("url"));
    }
    if color.len() != 7
        || !color.starts_with('#')
        || !color.bytes().skip(1).all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("color"));
    }
    if interval < 15 {
        return Err(invalid("interval_min"));
    }
    if lead.is_some_and(|n| n > 10080) {
        return Err(invalid("default_lead_min"));
    }
    let changed_url = old.is_some_and(|s| s.url != url);
    let (key,queued)=hub.db.transaction(|db|{
        let rows=db.exec("INSERT INTO sources(id,name,url,color,enabled,interval_min,default_lead_min,honor_valarm,notify)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
            ON CONFLICT(id) DO UPDATE SET name=excluded.name,url=excluded.url,color=excluded.color,enabled=excluded.enabled,
            interval_min=excluded.interval_min,default_lead_min=excluded.default_lead_min,honor_valarm=excluded.honor_valarm,notify=excluded.notify
            RETURNING id", &[old.map_or(V::Null,|s|V::Integer(s.id)),V::Text(name),V::Text(url),V::Text(color),
            V::Integer(i64::from(enabled)),V::Integer(i64::from(interval)),lead.map_or(V::Null,|n|V::Integer(i64::from(n))),
            V::Integer(i64::from(honor)),V::Integer(i64::from(notify))])?;
        let key=rows.first().ok_or_else(||CoreError::Internal("missing source id".into()))?.i64("id")?;
        if changed_url{db.exec("UPDATE sources SET etag=NULL,last_modified=NULL,last_sync_at=NULL,last_error=NULL,last_result=NULL,consecutive_failures=0,alerted=0 WHERE id=?1",&[V::Integer(key)])?;}
        let queued=if old.is_some(){enqueue_where(db,SeriesFilter::Source(key),"source_settings",hub.now())?}else{0};
        Ok((key,queued))
    })?;
    ok(json!({"id":key,"queued":queued}))
}
