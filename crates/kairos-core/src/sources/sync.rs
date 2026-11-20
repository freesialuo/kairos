//! Refresh feed snapshots and report sustained source failures.

use std::{
    collections::{BTreeSet, HashSet},
    panic::AssertUnwindSafe,
};

use futures::FutureExt;

use super::model::{SourceRow, source_from_row};
use crate::{
    CoreError, Db, Hub, NetError, Ports,
    calendar::events::upsert_event,
    ics::parse::{IcsError, ParsedFeed, parse_ics},
    notify::{
        bark::{load_channel, send_bark},
        message::{Message, format_source_alert},
    },
    ports::{Http, OutRequest, OutResponse, Value},
    sched::{alarm::StepOutcome, queue::enqueue_series},
};

pub const MAX_ICS_BYTES: usize = 5 * 1024 * 1024;
const FEED_TOO_LARGE: &str = "feed too large (> 5 MB)";
const INTERRUPTED: &str = "interrupted before completion (CPU or memory limit?)";
const CURRENT_ATTEMPT: &str =
    "id = ?1 AND last_sync_at = ?2 AND url = ?3 AND consecutive_failures = ?4";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSummary {
    pub events: u32,
    pub enqueued: u32,
    pub unchanged: bool,
}

pub fn pick_due_source(db: &impl Db, now: i64) -> Result<Option<SourceRow>, CoreError> {
    db.exec(
        "SELECT * FROM sources WHERE enabled = 1
         AND (last_sync_at IS NULL OR last_sync_at + interval_min * 60000 <= ?1)
         ORDER BY last_sync_at + interval_min * 60000, id LIMIT 1",
        &[Value::Integer(now)],
    )?
    .first()
    .map(source_from_row)
    .transpose()
}

pub async fn sync_due_source<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome {
    let run = async {
        let Some(source) = pick_due_source(&hub.db, now)? else {
            return Ok(StepOutcome::default());
        };
        let attempt = begin_sync(&hub.db, &source, now)?;
        if finish_sync(hub, &attempt).await.is_ok() {
            Ok(StepOutcome {
                ok: 1,
                ..StepOutcome::default()
            })
        } else {
            Ok(StepOutcome {
                failed: 1,
                ..StepOutcome::default()
            })
        }
    };
    run.await.unwrap_or_else(|error: CoreError| StepOutcome {
        step_error: Some(error.to_string()),
        ..StepOutcome::default()
    })
}

pub async fn sync_source<P: Ports>(
    hub: &Hub<P>,
    source: &SourceRow,
    now: i64,
) -> Result<SyncSummary, String> {
    let attempt = begin_sync(&hub.db, source, now).map_err(|error| error.to_string())?;
    finish_sync(hub, &attempt).await
}

struct Attempt<'a> {
    source_id: i64,
    url: &'a str,
    started_at: i64,
    failures: i64,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl Attempt<'_> {
    fn bindings(&self) -> Vec<Value> {
        vec![
            Value::Integer(self.source_id),
            Value::Integer(self.started_at),
            Value::Text(self.url.to_owned()),
            Value::Integer(self.failures),
        ]
    }
}

fn begin_sync<'a>(db: &impl Db, source: &'a SourceRow, now: i64) -> Result<Attempt<'a>, CoreError> {
    let rows = db.exec(
        "UPDATE sources SET last_sync_at = ?2,
         consecutive_failures = consecutive_failures + 1, last_error = ?4
         WHERE id = ?1 AND url = ?3 RETURNING consecutive_failures, etag, last_modified",
        &[
            Value::Integer(source.id),
            Value::Integer(now),
            Value::Text(source.url.clone()),
            Value::Text(INTERRUPTED.to_owned()),
        ],
    )?;
    let trace = rows.first().ok_or(CoreError::NotFound)?;
    Ok(Attempt {
        source_id: source.id,
        url: &source.url,
        started_at: now,
        failures: trace.i64("consecutive_failures")?,
        etag: trace.opt_text("etag")?,
        last_modified: trace.opt_text("last_modified")?,
    })
}

async fn finish_sync<P: Ports>(hub: &Hub<P>, attempt: &Attempt<'_>) -> Result<SyncSummary, String> {
    let attempted = async {
        let response = fetch_feed(hub, attempt).await?;
        let feed = if response.status == 304 {
            None
        } else {
            Some(
                parse_ics(&response.body, attempt.source_id).map_err(|error| match error {
                    IcsError::NotCalendar => "not a calendar (no VCALENDAR)",
                    IcsError::InvalidUtf8 => "feed is not valid UTF-8",
                    IcsError::InvalidEvent(reason) => reason,
                })?,
            )
        };
        save_feed(&hub.db, attempt, &response, feed).map_err(|error| error.to_string())
    };
    let result = AssertUnwindSafe(attempted)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err("panic while syncing source".into()));

    if let Err(error) = &result {
        let mut bindings = attempt.bindings();
        bindings.push(Value::Text(error.clone()));
        hub.db
            .exec(
                &format!("UPDATE sources SET last_error = ?5 WHERE {CURRENT_ATTEMPT}"),
                &bindings,
            )
            .map_err(|record| format!("{error} (could not record sync failure: {record})"))?;
    }
    result
}

async fn fetch_feed<P: Ports>(hub: &Hub<P>, attempt: &Attempt<'_>) -> Result<OutResponse, String> {
    let https = attempt
        .url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
    let http = attempt
        .url
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));
    if !(https || (http && hub.config.allow_http_urls)) {
        return Err("source URL must use HTTPS".into());
    }

    let headers = [
        ("If-None-Match", attempt.etag.as_ref()),
        ("If-Modified-Since", attempt.last_modified.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| Some((name.into(), value?.clone())))
    .collect();
    // The outbound await commits the pessimistic trace before feed parsing.
    let response = hub
        .http
        .send(OutRequest {
            method: "GET".into(),
            url: attempt.url.to_owned(),
            headers,
            body: None,
            max_body_bytes: MAX_ICS_BYTES,
        })
        .await
        .map_err(|error| match error {
            NetError::TooLarge => FEED_TOO_LARGE.to_owned(),
            NetError::Network(_) => error.to_string(),
        })?;

    if response.status == 304 {
        return Ok(response);
    }
    if !(200..300).contains(&response.status) {
        return Err(format!("HTTP {}", response.status));
    }
    if response.body.len() > MAX_ICS_BYTES
        || response.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("Content-Length") && value.split(',').any(too_large)
        })
    {
        return Err(FEED_TOO_LARGE.into());
    }
    Ok(response)
}

fn too_large(value: &str) -> bool {
    let digits = value.trim().trim_start_matches('0');
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits
            .parse::<usize>()
            .map_or(true, |bytes| bytes > MAX_ICS_BYTES)
}

fn response_header<'a>(response: &'a OutResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn save_feed(
    db: &impl Db,
    attempt: &Attempt<'_>,
    response: &OutResponse,
    feed: Option<ParsedFeed>,
) -> Result<SyncSummary, CoreError> {
    let unchanged = feed.is_none();
    let etag = response_header(response, "ETag");
    let modified = response_header(response, "Last-Modified");
    let (etag, modified) = if unchanged {
        (
            etag.or(attempt.etag.as_deref()),
            modified.or(attempt.last_modified.as_deref()),
        )
    } else {
        (etag, modified)
    };
    let events = u32::try_from(feed.as_ref().map_or(0, |p| p.rows.len()))
        .map_err(|err| CoreError::Internal(err.to_string()))?;
    let result = match &feed {
        None => "unchanged".into(),
        Some(p) if p.warnings.is_empty() => format!("ok: {events} events"),
        Some(p) => format!("ok: {events} events; {}", p.warnings.join("; ")),
    };

    db.transaction(|tx| {
        let mut bindings = attempt.bindings();
        bindings.extend([
            etag.map_or(Value::Null, |value| Value::Text(value.to_owned())),
            modified.map_or(Value::Null, |value| Value::Text(value.to_owned())),
            Value::Text(result),
        ]);
        let updated = tx.exec(
            &format!(
                "UPDATE sources SET etag = ?5, last_modified = ?6, last_result = ?7,
             consecutive_failures = 0, alerted = 0, last_error = NULL
             WHERE {CURRENT_ATTEMPT} RETURNING id"
            ),
            &bindings,
        )?;
        // An edit, deletion or newer sync can supersede a response while it is in flight.
        if updated.is_empty() {
            return Err(CoreError::Internal("source changed during sync".into()));
        }
        let enqueued = feed.map_or(Ok(0), |feed| save_rows(tx, attempt, feed))?;
        Ok(SyncSummary {
            events,
            enqueued,
            unchanged,
        })
    })
}

fn save_rows(db: &impl Db, attempt: &Attempt<'_>, feed: ParsedFeed) -> Result<u32, CoreError> {
    let mut present = HashSet::with_capacity(feed.rows.len());
    let mut changed = BTreeSet::new();
    for row in feed.rows {
        let update = upsert_event(db, &row, attempt.started_at)?;
        present.insert(update.id);
        if update.created || update.changed {
            changed.insert(row.series_key);
        }
    }
    for row in db.exec(
        "SELECT id, series_key FROM events WHERE source_id = ?1 AND origin = 'ics'",
        &[Value::Integer(attempt.source_id)],
    )? {
        let id = row.i64("id")?;
        if !present.contains(&id) {
            db.exec("DELETE FROM events WHERE id = ?1", &[Value::Integer(id)])?;
            changed.insert(row.text("series_key")?);
        }
    }
    for series_key in &changed {
        enqueue_series(db, series_key, "sync", attempt.started_at)?;
    }
    u32::try_from(changed.len()).map_err(|err| CoreError::Internal(err.to_string()))
}

pub async fn send_source_alerts<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome {
    let mut result = StepOutcome::default();
    let run = async {
        let sources = hub.db.exec(
            "SELECT id FROM sources WHERE consecutive_failures >= 3 AND alerted = 0 ORDER BY id",
            &[],
        )?;
        for source in sources {
            let traced = hub.db.exec(
                "UPDATE sources SET alerted = 1 WHERE id = ?1
                 AND consecutive_failures >= 3 AND alerted = 0 RETURNING name, last_error",
                &[Value::Integer(source.i64("id")?)],
            )?;
            let Some(source) = traced.first() else {
                continue;
            };
            let name = source.text("name")?;
            let error = source.opt_text("last_error")?;
            let msg = format_source_alert(
                &name,
                error.as_deref().unwrap_or("unknown sync error"),
                &hub.config.origin,
            );
            let sent = AssertUnwindSafe(send_alert(hub, &msg))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err("panic while sending source alert".into()));
            let ok = sent.is_ok();
            hub.db.exec(
                "INSERT INTO deliveries (at, kind, channel, ok, error, summary)
                 VALUES (?1, 'source_alert', 'bark', ?2, ?3, ?4)",
                &[
                    Value::Integer(now),
                    Value::Integer(i64::from(ok)),
                    sent.err().map_or(Value::Null, Value::Text),
                    Value::Text(msg.body),
                ],
            )?;
            if ok {
                result.ok += 1;
            } else {
                result.failed += 1;
            }
        }
        Ok::<(), CoreError>(())
    };
    if let Err(error) = run.await {
        result.step_error = Some(error.to_string());
    }
    result
}

async fn send_alert<P: Ports>(hub: &Hub<P>, msg: &Message) -> Result<(), String> {
    let channel = load_channel(&hub.db)
        .map_err(|error| error.to_string())?
        .ok_or("no notification channel configured")?;
    send_bark(&hub.http, &channel, msg)
        .await
        .map_err(|err| err.to_string())
}
