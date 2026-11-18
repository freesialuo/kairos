use super::{
    cookies::{cookie, set_cookie},
    crypto::{b64url, b64url_decode, random_bytes, sha256_hex},
};
use crate::{CoreError, Db, Hub, Ports, http::Request, ports::Value};

pub use kairos_types::auth::SessionScope as Scope;

pub const SESSION_COOKIE: &str = "__Host-sid";
pub const SESSION_TTL_MS: i64 = 30 * 86_400_000;
const EXTEND_AFTER_MS: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id_hash: String,
    pub scope: Scope,
    pub expires_at: i64,
}

pub fn create_session<P: Ports>(
    hub: &Hub<P>,
    scope: Scope,
) -> Result<(Session, String), CoreError> {
    insert_session(&hub.db, &hub.entropy, hub.now(), scope)
}

pub(crate) fn insert_session(
    db: &impl Db,
    entropy: &impl crate::ports::Entropy,
    now: i64,
    scope: Scope,
) -> Result<(Session, String), CoreError> {
    let bytes = random_bytes::<32>(entropy);
    let session = Session {
        id_hash: sha256_hex(&bytes),
        scope,
        expires_at: now.saturating_add(SESSION_TTL_MS),
    };
    let value = match scope {
        Scope::Full => "full",
        Scope::Recovery => "recovery",
    };
    db.exec(
        "INSERT INTO sessions(id_hash,scope,created_at,expires_at,last_extended_at)
         VALUES (?1,?2,?3,?4,?3)",
        &[
            Value::Text(session.id_hash.clone()),
            Value::Text(value.into()),
            Value::Integer(now),
            Value::Integer(session.expires_at),
        ],
    )?;
    Ok((session, b64url(&bytes)))
}

pub fn read_session<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Option<Session>, CoreError> {
    let Some(token) = cookie(req, SESSION_COOKIE).filter(|value| value.len() == 43) else {
        return Ok(None);
    };
    let Ok(bytes) = b64url_decode(&token) else {
        return Ok(None);
    };
    let id_hash = sha256_hex(&bytes);
    let now = hub.now();
    let rows = hub.db.exec(
        "SELECT scope,expires_at,last_extended_at FROM sessions WHERE id_hash=?1 AND expires_at>?2",
        &[Value::Text(id_hash.clone()), Value::Integer(now)],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let scope = match row.text("scope")?.as_str() {
        "full" => Scope::Full,
        "recovery" => Scope::Recovery,
        _ => return Err(CoreError::Internal("invalid stored session scope".into())),
    };
    let mut expires_at = row.i64("expires_at")?;
    let last_extended_at = row.i64("last_extended_at")?;
    if now.saturating_sub(last_extended_at) >= EXTEND_AFTER_MS {
        expires_at = now.saturating_add(SESSION_TTL_MS);
        hub.db.exec(
            "UPDATE sessions SET expires_at=?2,last_extended_at=?3 WHERE id_hash=?1 AND last_extended_at=?4",
            &[Value::Text(id_hash.clone()), Value::Integer(expires_at), Value::Integer(now),
                Value::Integer(last_extended_at)],
        )?;
    }
    Ok(Some(Session {
        id_hash,
        scope,
        expires_at,
    }))
}

pub fn require_full_session<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Session, CoreError> {
    let session = read_session(hub, req)?.ok_or(CoreError::Unauthorized)?;
    if session.scope != Scope::Full {
        return Err(CoreError::Forbidden);
    }
    Ok(session)
}

pub fn session_cookie_for_request(
    req: &Request,
    session: &Session,
    now: i64,
) -> Option<(String, String)> {
    let token = cookie(req, SESSION_COOKIE)?;
    let remaining = session.expires_at.saturating_sub(now);
    if remaining <= 0 {
        return None;
    }
    let seconds = remaining / 1000 + i64::from(remaining % 1000 != 0);
    Some(set_cookie(SESSION_COOKIE, &token, Some(seconds)))
}

pub fn upgrade_session(db: &impl Db, session: &Session) -> Result<(), CoreError> {
    db.exec(
        "UPDATE sessions SET scope='full' WHERE id_hash=?1 AND scope='recovery'",
        &[Value::Text(session.id_hash.clone())],
    )?;
    Ok(())
}

pub fn revoke_session(db: &impl Db, session: &Session) -> Result<(), CoreError> {
    db.exec(
        "DELETE FROM sessions WHERE id_hash=?1",
        &[Value::Text(session.id_hash.clone())],
    )?;
    Ok(())
}

pub fn revoke_all_sessions(db: &impl Db) -> Result<(), CoreError> {
    db.exec("DELETE FROM sessions", &[])?;
    Ok(())
}
