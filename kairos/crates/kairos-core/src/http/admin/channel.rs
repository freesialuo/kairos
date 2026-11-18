use super::*;
use crate::notify::{
    bark::{BarkConfig, load_channel, send_bark},
    message::Message,
};

pub(super) fn masked(key: &str) -> String {
    let tail = key.chars().rev().take(4).collect::<Vec<_>>();
    if key.chars().count() <= 4 {
        "••••".into()
    } else {
        format!("••••{}", tail.into_iter().rev().collect::<String>())
    }
}

pub(super) fn view(db: &impl Db) -> Result<serde_json::Value, CoreError> {
    let rows = db.exec("SELECT enabled,config FROM channels WHERE kind='bark'", &[])?;
    if let Some(row) = rows.first() {
        let cfg: BarkConfig = serde_json::from_str(&row.text("config")?)
            .map_err(|_| CoreError::Internal("invalid Bark config".into()))?;
        Ok(
            json!({"server":cfg.server,"device_key":masked(&cfg.device_key),"enabled":row.bool("enabled")?}),
        )
    } else {
        Ok(json!({"server":"https://api.day.app","device_key":"","enabled":false}))
    }
}
pub(super) async fn route<P: Ports>(
    hub: &Hub<P>,
    req: &Request,
    parts: &[&str],
) -> Result<Response, CoreError> {
    match (req.method, parts) {
        (Method::Get, []) => ok(view(&hub.db)?),
        (Method::Put, []) => {
            #[derive(serde::Deserialize)]
            struct Input {
                server: String,
                device_key: Option<String>,
                enabled: bool,
            }
            let input: Input = body(req)?;
            if !valid_url(&input.server, hub.config.allow_http_urls) {
                return Err(invalid("server"));
            }
            let key = match input.device_key.filter(|s| !s.is_empty()) {
                Some(key) => key,
                None => {
                    let rows = hub
                        .db
                        .exec("SELECT config FROM channels WHERE kind='bark'", &[])?;
                    let stored = rows
                        .first()
                        .ok_or_else(|| invalid("device_key"))?
                        .text("config")?;
                    let previous: BarkConfig =
                        serde_json::from_str(&stored).map_err(|_| invalid("device_key"))?;
                    if input.server != previous.server {
                        return Err(invalid("device_key"));
                    }
                    previous.device_key
                }
            };
            let cfg = BarkConfig {
                server: input.server,
                device_key: key,
            };
            hub.db.exec(
                "INSERT INTO channels(kind,config,enabled) VALUES('bark',?1,?2)
                ON CONFLICT(kind) DO UPDATE SET config=excluded.config,enabled=excluded.enabled",
                &[
                    V::Text(json!(cfg).to_string()),
                    V::Integer(i64::from(input.enabled)),
                ],
            )?;
            ok(view(&hub.db)?)
        }
        (Method::Post, ["test"]) => {
            let cfg = load_channel(&hub.db)?.ok_or_else(|| invalid("channel"))?;
            let message = Message {
                title: "Kairos test".into(),
                body: "Your reminders are connected.".into(),
                url: None,
            };
            let sent = send_bark(&hub.http, &cfg, &message).await;
            let error = sent.as_ref().err().map(ToString::to_string);
            hub.db.exec("INSERT INTO deliveries(at,kind,channel,ok,error,summary) VALUES(?1,'test','bark',?2,?3,'Test notification')",
                &[V::Integer(hub.now()),V::Integer(i64::from(sent.is_ok())),error.clone().map_or(V::Null,V::Text)])?;
            Ok(Response::json(
                if sent.is_ok() { 200 } else { 502 },
                &json!({"ok":sent.is_ok(),"error":error}),
            ))
        }
        _ => Err(CoreError::NotFound),
    }
}
