use super::settings::{read_meta, set_meta};
use crate::{CoreError, Db};

pub const MIGRATIONS: &[&str] = &[
    r#"CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);          -- JSON values; 'meta:*' keys are internal
CREATE TABLE sources (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, color TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1, interval_min INTEGER NOT NULL DEFAULT 30, default_lead_min INTEGER,
  honor_valarm INTEGER NOT NULL DEFAULT 1, etag TEXT, last_modified TEXT, last_sync_at INTEGER,
  last_result TEXT, last_error TEXT, consecutive_failures INTEGER NOT NULL DEFAULT 0,
  alerted INTEGER NOT NULL DEFAULT 0,             -- 1 once the "sync failing" alert went out (§7.5)
  notify INTEGER NOT NULL DEFAULT 1);             -- 0 = this source's events never produce reminders (§7.6)
CREATE TABLE events (
  id INTEGER PRIMARY KEY, origin TEXT NOT NULL, source_id INTEGER, series_key TEXT NOT NULL,
  uid TEXT, recurrence_id TEXT, external_id TEXT, title TEXT NOT NULL, description TEXT, location TEXT,
  all_day INTEGER NOT NULL, start_local TEXT NOT NULL, tzid TEXT, duration_sec INTEGER NOT NULL, rrule TEXT,
  exdates TEXT NOT NULL DEFAULT '[]', lead_minutes TEXT, valarm_minutes TEXT NOT NULL DEFAULT '[]',
  hash TEXT NOT NULL, updated_at INTEGER NOT NULL,
  expand_anchor TEXT,              -- JSON {slot, index, hash}: resume point for recurrence expansion (§7.4)
  expand_status TEXT NOT NULL DEFAULT 'complete',   -- 'complete' | 'catching_up' | 'too_dense'
  expand_warning TEXT);
CREATE UNIQUE INDEX events_identity ON events(series_key, COALESCE(recurrence_id, ''));
CREATE UNIQUE INDEX events_external ON events(external_id) WHERE external_id IS NOT NULL;
CREATE INDEX events_source ON events(source_id);
CREATE TABLE reminders (
  key TEXT PRIMARY KEY, series_key TEXT NOT NULL, event_id INTEGER NOT NULL,
  occ_start_utc INTEGER NOT NULL, lead_min INTEGER, fire_at INTEGER NOT NULL, next_attempt_at INTEGER NOT NULL,
  status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT, sent_at INTEGER,
  late INTEGER NOT NULL DEFAULT 0);
CREATE INDEX reminders_due ON reminders(status, next_attempt_at);
CREATE INDEX reminders_series ON reminders(series_key);
CREATE TABLE reconcile_queue (
  series_key TEXT PRIMARY KEY, reason TEXT NOT NULL, enqueued_at INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0, not_before INTEGER NOT NULL, last_error TEXT);
CREATE INDEX reconcile_queue_due ON reconcile_queue(not_before);
CREATE TABLE deliveries (
  id INTEGER PRIMARY KEY, at INTEGER NOT NULL, reminder_key TEXT, kind TEXT NOT NULL,   -- 'reminder' | 'source_alert' | 'test'
  channel TEXT NOT NULL, ok INTEGER NOT NULL, error TEXT, summary TEXT NOT NULL);
CREATE TABLE channels (kind TEXT PRIMARY KEY, config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE passkeys (
  id TEXT PRIMARY KEY,                              -- base64url credential id
  public_key BLOB NOT NULL,                         -- COSE_Key bytes as received
  alg INTEGER NOT NULL,                             -- -7 | -8 | -257
  counter INTEGER NOT NULL, transports TEXT,
  rp_id TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE sessions (
  id_hash TEXT PRIMARY KEY, scope TEXT NOT NULL, created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, last_extended_at INTEGER NOT NULL);
CREATE TABLE api_tokens (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, hash TEXT NOT NULL UNIQUE, prefix TEXT NOT NULL,
  created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE auth_failures (at INTEGER NOT NULL);
"#,
    "ALTER TABLE events ADD COLUMN initial_start_utc INTEGER;",
    // The original DTEND is unavailable in stored events. Fetch a full snapshot
    // on the next regular sync to recalculate zoned durations.
    "UPDATE sources SET etag = NULL, last_modified = NULL
     WHERE (etag IS NOT NULL OR last_modified IS NOT NULL) AND EXISTS (
       SELECT 1 FROM events WHERE source_id = sources.id AND origin = 'ics'
       AND all_day = 0 AND tzid IS NOT NULL
     );",
];

pub fn migrate(db: &impl Db) -> Result<(), CoreError> {
    let has_settings = !db
        .exec(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='settings'",
            &[],
        )?
        .is_empty();
    let version = if has_settings {
        read_meta(db, "schema_version")?
            .map(|v| v.parse::<usize>())
            .transpose()
            .map_err(|_| CoreError::Internal("invalid schema version".into()))?
            .unwrap_or(0)
    } else {
        0
    };
    let pending = MIGRATIONS
        .get(version..)
        .ok_or_else(|| CoreError::Internal("database schema is newer than this build".into()))?;
    for (index, migration) in (version + 1..).zip(pending) {
        db.transaction(|tx| {
            // Migration DDL has one statement terminator per closing line. Strip
            // SQL line comments before checking it (some comments contain ';').
            let mut statement = String::new();
            for line in migration.lines() {
                let sql = line
                    .split_once("--")
                    .map_or(line, |(ddl, _)| ddl)
                    .trim_end();
                statement.push_str(sql);
                statement.push('\n');
                if sql.ends_with(';') {
                    tx.exec(&statement, &[])?;
                    statement.clear();
                }
            }
            if !statement.trim().is_empty() {
                return Err(CoreError::Internal(
                    "unterminated migration statement".into(),
                ));
            }
            set_meta(tx, "schema_version", &index.to_string())
        })?;
    }
    Ok(())
}
