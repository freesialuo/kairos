use crate::{
    CoreError, Db, DbError, Hub, Ports,
    auth::crypto::{b64url, b64url_decode, random_bytes, sha256_hex},
    http::Request,
    ports::Value,
};

pub use kairos_types::api_v1::TokenInfo;

const PREFIX: &str = "kr_";
const PREFIX_VISIBLE: usize = 10;
const TOKEN_BYTES: usize = 32;
const HOUR_MS: i64 = 3_600_000;

pub fn create_token<P: Ports>(hub: &Hub<P>, name: &str) -> Result<(i64, String), CoreError> {
    if name.trim().is_empty() {
        return Err(CoreError::Invalid {
            field: "name",
            message: "the name is empty".into(),
        });
    }
    let token = format!(
        "{PREFIX}{}",
        b64url(&random_bytes::<TOKEN_BYTES>(&hub.entropy))
    );
    let rows = hub.db.exec(
        "INSERT INTO api_tokens(name,hash,prefix,created_at) VALUES(?1,?2,?3,?4) RETURNING id",
        &[
            Value::Text(name.into()),
            Value::Text(sha256_hex(token.as_bytes())),
            Value::Text(token.chars().take(PREFIX_VISIBLE).collect()),
            Value::Integer(hub.now()),
        ],
    )?;
    let id = rows
        .first()
        .ok_or_else(|| DbError("token insert did not return an id".into()))?
        .i64("id")?;
    Ok((id, token))
}

pub fn verify_bearer<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<i64, CoreError> {
    let mut headers = req
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"));
    let (_, value) = headers.next().ok_or(CoreError::Unauthorized)?;
    if headers.next().is_some() {
        return Err(CoreError::Unauthorized);
    }
    let mut fields = value.split_ascii_whitespace();
    let scheme = fields.next().ok_or(CoreError::Unauthorized)?;
    let token = fields.next().ok_or(CoreError::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") || fields.next().is_some() {
        return Err(CoreError::Unauthorized);
    }
    let encoded = token.strip_prefix(PREFIX).ok_or(CoreError::Unauthorized)?;
    if encoded.len() != 43
        || b64url_decode(encoded)
            .map_err(|_| CoreError::Unauthorized)?
            .len()
            != TOKEN_BYTES
    {
        return Err(CoreError::Unauthorized);
    }
    let found = hub.db.exec(
        "SELECT id,last_used_at FROM api_tokens WHERE hash=?1",
        &[Value::Text(sha256_hex(token.as_bytes()))],
    )?;
    let row = found.first().ok_or(CoreError::Unauthorized)?;
    let id = row.i64("id")?;
    let now = hub.now();
    let stale = now.saturating_sub(HOUR_MS);
    if row
        .opt_i64("last_used_at")?
        .is_none_or(|last| last <= stale)
    {
        hub.db.exec(
            "UPDATE api_tokens SET last_used_at=?2 WHERE id=?1 AND (last_used_at IS NULL OR last_used_at<=?3)",
            &[Value::Integer(id), Value::Integer(now), Value::Integer(stale)],
        )?;
    }
    Ok(id)
}

pub fn list_tokens(db: &impl Db) -> Result<Vec<TokenInfo>, CoreError> {
    db.exec(
        "SELECT id,name,prefix,created_at,last_used_at FROM api_tokens ORDER BY id DESC",
        &[],
    )?
    .iter()
    .map(|row| {
        Ok(TokenInfo {
            id: row.i64("id")?,
            name: row.text("name")?,
            prefix: row.text("prefix")?,
            created_at: row.i64("created_at")?,
            last_used_at: row.opt_i64("last_used_at")?,
        })
    })
    .collect()
}

pub fn revoke_token(db: &impl Db, id: i64) -> Result<(), CoreError> {
    if db
        .exec(
            "DELETE FROM api_tokens WHERE id=?1 RETURNING id",
            &[Value::Integer(id)],
        )?
        .is_empty()
    {
        return Err(CoreError::NotFound);
    }
    Ok(())
}
