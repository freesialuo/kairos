use crate::{
    CoreError, Db, DbError,
    ports::{Row, Value},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: i64,
    pub name: String,
    pub url: String,
    pub color: String,
    pub enabled: bool,
    pub interval_min: u32,
    pub default_lead_min: Option<u32>,
    pub honor_valarm: bool,
    pub notify: bool,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_sync_at: Option<i64>,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
    pub alerted: bool,
}

pub fn get_source(db: &impl Db, id: i64) -> Result<Option<SourceRow>, CoreError> {
    db.exec("SELECT * FROM sources WHERE id = ?1", &[Value::Integer(id)])?
        .first()
        .map(source_from_row)
        .transpose()
}

pub fn list_sources(db: &impl Db) -> Result<Vec<SourceRow>, CoreError> {
    db.exec("SELECT * FROM sources ORDER BY id", &[])?
        .iter()
        .map(source_from_row)
        .collect()
}

pub(super) fn source_from_row(row: &Row) -> Result<SourceRow, CoreError> {
    Ok(SourceRow {
        id: row.i64("id")?,
        name: row.text("name")?,
        url: row.text("url")?,
        color: row.text("color")?,
        enabled: row.bool("enabled")?,
        interval_min: unsigned(row.i64("interval_min")?, "interval_min")?,
        default_lead_min: row
            .opt_i64("default_lead_min")?
            .map(|value| unsigned(value, "default_lead_min"))
            .transpose()?,
        honor_valarm: row.bool("honor_valarm")?,
        notify: row.bool("notify")?,
        etag: row.opt_text("etag")?,
        last_modified: row.opt_text("last_modified")?,
        last_sync_at: row.opt_i64("last_sync_at")?,
        last_result: row.opt_text("last_result")?,
        last_error: row.opt_text("last_error")?,
        consecutive_failures: unsigned(row.i64("consecutive_failures")?, "consecutive_failures")?,
        alerted: row.bool("alerted")?,
    })
}

fn unsigned(value: i64, column: &str) -> Result<u32, DbError> {
    u32::try_from(value).map_err(|_| DbError(format!("integer out of range in {column}")))
}
