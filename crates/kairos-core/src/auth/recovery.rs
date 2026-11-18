use super::crypto::{ct_eq, sha256};
use crate::{
    CoreError, Db, Hub, Ports,
    db::settings::{read_meta, set_meta},
    ports::Value,
};

const LOCK_KEY: &str = "recovery_locked_until";
const HOUR_MS: i64 = 3_600_000;
const MAX_FAILURES: i64 = 5;

pub fn check_recovery_code<P: Ports>(hub: &Hub<P>, code: &str) -> Result<(), CoreError> {
    let now = hub.now();
    hub.db.transaction(|db| {
        if let Some(until) = read_meta(db, LOCK_KEY)? {
            let until: i64 = until
                .parse()
                .map_err(|_| CoreError::Internal("invalid recovery lock".into()))?;
            if now < until {
                return Ok(Err(CoreError::TooManyRequests));
            }
        }
        if ct_eq(&sha256(code.as_bytes()), &hub.config.recovery_digest) {
            return Ok(Ok(()));
        }
        db.exec(
            "DELETE FROM auth_failures WHERE at<=?1",
            &[Value::Integer(now.saturating_sub(HOUR_MS))],
        )?;
        db.exec(
            "INSERT INTO auth_failures(at) VALUES (?1)",
            &[Value::Integer(now)],
        )?;
        let failures = db
            .exec("SELECT COUNT(*) AS n FROM auth_failures", &[])?
            .first()
            .ok_or_else(|| CoreError::Internal("missing recovery failure count".into()))?
            .i64("n")?;
        if failures >= MAX_FAILURES {
            set_meta(db, LOCK_KEY, &now.saturating_add(HOUR_MS).to_string())?;
        }
        Ok(Err(CoreError::Unauthorized))
    })?
}
