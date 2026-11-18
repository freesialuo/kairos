//! Local adapter probes, compiled only for the test Worker.
use super::{Method, Request, Response};
use crate::{
    CoreError, Db, Hub, NetError, Ports,
    ports::{Alarm, Commit, Entropy, Http, OutRequest},
};
use serde::Deserialize;
use serde_json::json;

fn payload<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T, CoreError> {
    serde_json::from_slice(&req.body).map_err(|error| CoreError::Invalid {
        field: "body",
        message: error.to_string(),
    })
}

pub(super) async fn adapter<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let result = match (req.method, req.path.as_str()) {
        (Method::Post, "/__test/clock") => {
            #[derive(Deserialize)]
            struct Offset {
                offset_ms: i64,
            }
            let offset: Offset = payload(req)?;
            crate::db::settings::set_meta(
                &hub.db,
                "test_clock_offset_ms",
                &offset.offset_ms.to_string(),
            )?;
            json!({"now":hub.now()})
        }
        (Method::Post, "/__test/alarm") => serde_json::to_value(hub.run_alarm().await)
            .map_err(|e| CoreError::Internal(e.to_string()))?,
        (Method::Post, "/__test/adapter/tx-rollback") => {
            #[derive(Deserialize)]
            struct Body {
                #[serde(default)]
                panic: bool,
            }
            let body: Body = payload(req)?;
            // Compare both sides of the transaction without disturbing application settings.
            let key = "test:adapter-rollback";
            let params = [crate::ports::Value::Text(key.into())];
            let before = hub
                .db
                .exec("SELECT value FROM settings WHERE key=?1", &params)?;
            let rollback = || CoreError::Internal("intentional rollback".into());
            let result: Result<(), CoreError> = hub.db.transaction(|tx| {
                tx.exec(
                    "INSERT OR REPLACE INTO settings(key,value) VALUES (?1, 'probe')",
                    &params,
                )?;
                if tx
                    .exec("SELECT value FROM settings WHERE key=?1", &params)?
                    .len()
                    != 1
                {
                    return Err(CoreError::Internal("probe write was not visible".into()));
                }
                if body.panic {
                    std::panic::resume_unwind(Box::new("intentional transaction panic"));
                }
                Err(rollback())
            });
            match result {
                Err(error) if error == rollback() => (),
                Err(error) => return Err(error),
                Ok(()) => return Err(CoreError::Internal("unexpected commit".into())),
            }
            let after = hub
                .db
                .exec("SELECT value FROM settings WHERE key=?1", &params)?;
            json!({"rolled_back": before.is_empty() && after.is_empty()})
        }
        (Method::Post, "/__test/adapter/commit") => {
            // Exercise the Worker WebCrypto backend before acknowledging durability.
            hub.entropy.fill(&mut [0; 32]);
            hub.commit.commit_point().await?;
            json!({"ok": true})
        }
        (Method::Post, "/__test/adapter/alarm") => {
            #[derive(Deserialize)]
            struct Body {
                at: i64,
            }
            let body: Body = payload(req)?;
            hub.alarm.set(body.at).await?;
            json!({"at": hub.alarm.get().await?})
        }
        (Method::Get, "/__test/adapter/alarm") => json!({"at": hub.alarm.get().await?}),
        (Method::Get, "/__test/adapter/schema") => {
            json!({"version": crate::db::settings::get_meta(&hub.db, "schema_version")})
        }
        (Method::Get, "/__test/adapter/watchdog") => {
            json!({"at": crate::db::settings::read_meta(&hub.db, "last_watchdog_at")?})
        }
        (Method::Post, "/__test/adapter/fetch") => {
            #[derive(Deserialize)]
            struct Body {
                url: String,
                max: usize,
            }
            let body: Body = payload(req)?;
            match hub
                .http
                .send(OutRequest {
                    method: "GET".into(),
                    url: body.url,
                    headers: vec![],
                    body: None,
                    max_body_bytes: body.max,
                })
                .await
            {
                Ok(response) => json!({"status": response.status, "len": response.body.len()}),
                Err(NetError::TooLarge) => json!({"error": "too_large"}),
                Err(error) => return Err(error.into()),
            }
        }
        _ => return Err(CoreError::NotFound),
    };
    Ok(Response::json(200, &result))
}
