use crate::ports::Value;
use crate::{CoreError, Db};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Settings {
    pub timezone: String,
    pub default_lead_min: u32,
    pub all_day_time: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            timezone: "UTC".into(),
            default_lead_min: 15,
            all_day_time: "09:00".into(),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, CoreError> {
    serde_json::from_str(s)
        .map_err(|error| CoreError::Internal(format!("invalid stored setting: {error}")))
}

pub fn get_settings(db: &impl Db) -> Result<Settings, CoreError> {
    let mut settings = Settings::default();
    for row in db.exec(
        "SELECT key,value FROM settings WHERE key IN ('timezone','defaultLeadMin','allDayTime')",
        &[],
    )? {
        let value = row.text("value")?;
        match row.text("key")?.as_str() {
            "timezone" => settings.timezone = decode(&value)?,
            "defaultLeadMin" => settings.default_lead_min = decode(&value)?,
            "allDayTime" => settings.all_day_time = decode(&value)?,
            _ => (),
        }
    }
    Ok(settings)
}

pub fn put_settings(db: &impl Db, settings: &Settings) -> Result<(), CoreError> {
    db.exec("INSERT INTO settings(key,value) VALUES ('timezone',?1),('defaultLeadMin',?2),('allDayTime',?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        &[Value::Text(serde_json::Value::from(settings.timezone.as_str()).to_string()),
          Value::Text(settings.default_lead_min.to_string()),
          Value::Text(serde_json::Value::from(settings.all_day_time.as_str()).to_string())])?;
    Ok(())
}

pub(crate) fn read_meta(db: &impl Db, key: &str) -> Result<Option<String>, CoreError> {
    db.exec(
        "SELECT value FROM settings WHERE key=?1",
        &[Value::Text(format!("meta:{key}"))],
    )?
    .first()
    .map(|row| decode(&row.text("value")?))
    .transpose()
}

pub fn get_meta(db: &impl Db, key: &str) -> Option<String> {
    read_meta(db, key).ok().flatten()
}

pub fn set_meta(db: &impl Db, key: &str, value: &str) -> Result<(), CoreError> {
    db.exec("INSERT INTO settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        &[Value::Text(format!("meta:{key}")), Value::Text(serde_json::Value::from(value).to_string())])?;
    Ok(())
}

pub fn del_meta(db: &impl Db, key: &str) -> Result<(), CoreError> {
    db.exec(
        "DELETE FROM settings WHERE key=?1",
        &[Value::Text(format!("meta:{key}"))],
    )?;
    Ok(())
}
