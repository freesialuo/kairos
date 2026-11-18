use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    CoreError, Db,
    ports::{Http, OutRequest},
};

use super::message::Message;

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct BarkConfig {
    #[serde(default = "default_server")]
    pub server: String,
    pub device_key: String,
}

fn default_server() -> String {
    "https://api.day.app".into()
}

impl fmt::Debug for BarkConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BarkConfig")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

pub fn load_channel(db: &impl Db) -> Result<Option<BarkConfig>, CoreError> {
    db.exec(
        "SELECT config FROM channels WHERE kind='bark' AND enabled=1",
        &[],
    )?
    .first()
    .map(|row| {
        serde_json::from_str(&row.text("config")?)
            .map_err(|_| CoreError::Internal("invalid Bark channel configuration".into()))
    })
    .transpose()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    Http(u16),
    Api(i64, String),
    Net(String),
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(status) => write!(f, "HTTP {status}"),
            Self::Api(code, message) => write!(f, "Bark {code}: {message}"),
            Self::Net(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ChannelError {}

pub async fn send_bark(
    http: &impl Http,
    cfg: &BarkConfig,
    msg: &Message,
) -> Result<(), ChannelError> {
    let payload = serde_json::json!({
        "device_key": cfg.device_key,
        "title": msg.title,
        "body": msg.body,
        "url": msg.url,
        "group": "reminders",
        "level": "timeSensitive",
    });
    let response = http
        .send(OutRequest {
            method: "POST".into(),
            url: format!("{}/push", cfg.server.trim_end_matches('/')),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(payload.to_string().into_bytes()),
            max_body_bytes: 65_536,
        })
        .await
        .map_err(|error| ChannelError::Net(error.to_string()))?;
    if response.status != 200 {
        return Err(ChannelError::Http(response.status));
    }
    let invalid = || ChannelError::Net("invalid Bark response".into());
    let reply: serde_json::Value = serde_json::from_slice(&response.body).map_err(|_| invalid())?;
    let code = reply
        .get("code")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(invalid)?;
    if code == 200 {
        return Ok(());
    }
    let message = reply
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    Err(ChannelError::Api(code, message.into()))
}
