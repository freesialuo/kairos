use serde::Deserialize;

use super::{
    cookies::{cookie, set_cookie},
    crypto::{b64url, b64url_decode, random_bytes, sha256_hex, sign, verify},
};
use crate::{CoreError, Db, Hub, Ports, http::Request, ports::Value};

pub const CHALLENGE_COOKIE: &str = "__Host-chal";
const TTL_MS: i64 = 120_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    Reg,
    Auth,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    purpose: Purpose,
    challenge: String,
    exp: i64,
}

pub fn issue_challenge<P: Ports>(hub: &Hub<P>, purpose: Purpose) -> ([u8; 32], (String, String)) {
    let challenge = random_bytes::<32>(&hub.entropy);
    let payload = serde_json::json!({
        "purpose": purpose,
        "challenge": b64url(&challenge),
        "exp": hub.now().saturating_add(TTL_MS),
    });
    let signed = sign(&hub.config.session_secret, payload.to_string().as_bytes());
    (
        challenge,
        set_cookie(CHALLENGE_COOKIE, &signed, Some(TTL_MS / 1000)),
    )
}

pub fn clear_challenge_cookie() -> (String, String) {
    set_cookie(CHALLENGE_COOKIE, "", Some(0))
}

pub fn take_challenge<P: Ports>(
    hub: &Hub<P>,
    req: &Request,
    purpose: Purpose,
) -> Result<([u8; 32], (String, String)), CoreError> {
    let token = cookie(req, CHALLENGE_COOKIE)
        .filter(|value| value.len() <= 256)
        .ok_or(CoreError::Unauthorized)?;
    let payload = verify(&hub.config.session_secret, &token).ok_or(CoreError::Unauthorized)?;
    let payload: Payload = serde_json::from_slice(&payload).map_err(|_| CoreError::Unauthorized)?;
    let now = hub.now();
    if payload.purpose != purpose || payload.exp <= now {
        return Err(CoreError::Unauthorized);
    }
    let challenge = b64url_decode(&payload.challenge)
        .ok()
        .and_then(|value| value.try_into().ok())
        .ok_or(CoreError::Unauthorized)?;
    // Persist only a temporary digest so a resent cookie cannot replay a ceremony.
    let used = hub.db.transaction(|db| {
        db.exec(
            "DELETE FROM settings WHERE key GLOB 'meta:auth_challenge:*' AND CAST(value AS INTEGER)<=?1",
            &[Value::Integer(now)],
        )?;
        let rows = db.exec(
            "INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT DO NOTHING RETURNING 1",
            &[Value::Text(format!("meta:auth_challenge:{}", sha256_hex(token.as_bytes()))),
                Value::Text(payload.exp.to_string())],
        )?;
        Ok(rows.is_empty())
    })?;
    if used {
        return Err(CoreError::Unauthorized);
    }
    Ok((challenge, clear_challenge_cookie()))
}
