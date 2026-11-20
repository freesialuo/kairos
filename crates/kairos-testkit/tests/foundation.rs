use kairos_core::{
    CoreError, Db,
    db::{
        schema::migrate,
        settings::{get_meta, get_settings, set_meta},
    },
};
use kairos_testkit::{Faults, SqliteDb, TestHub};

#[tokio::test]
async fn migration_creates_all_tables_and_is_idempotent() {
    let h = TestHub::new();
    h.migrate().unwrap(); // second run is a no-op
    let names: Vec<String> = h
        .sql("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .iter()
        .map(|r| r.text("name").unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "api_tokens",
            "auth_failures",
            "channels",
            "deliveries",
            "events",
            "passkeys",
            "reconcile_queue",
            "reminders",
            "sessions",
            "settings",
            "sources"
        ]
    );
    assert_eq!(get_meta(&h.db, "schema_version").as_deref(), Some("3"));
}

#[test]
fn migration_preserves_v1_event_rows() {
    let db = SqliteDb::new(Faults::default());
    db.exec(
        "CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT NOT NULL)",
        &[],
    )
    .unwrap();
    db.exec(
        "CREATE TABLE events(id INTEGER PRIMARY KEY,title TEXT NOT NULL,
         source_id INTEGER,origin TEXT,all_day INTEGER,tzid TEXT)",
        &[],
    )
    .unwrap();
    db.exec(
        "CREATE TABLE sources(id INTEGER PRIMARY KEY,etag TEXT,last_modified TEXT)",
        &[],
    )
    .unwrap();
    db.exec(
        "INSERT INTO events(id,title) VALUES(7,'Existing event')",
        &[],
    )
    .unwrap();
    set_meta(&db, "schema_version", "1").unwrap();

    migrate(&db).unwrap();
    let rows = db
        .exec("SELECT title,initial_start_utc FROM events WHERE id=7", &[])
        .unwrap();
    assert_eq!(rows[0].text("title").unwrap(), "Existing event");
    assert_eq!(rows[0].opt_i64("initial_start_utc").unwrap(), None);
    assert_eq!(get_meta(&db, "schema_version").as_deref(), Some("3"));
}
#[tokio::test]
async fn settings_have_spec_defaults() {
    let s = get_settings(&TestHub::new().db).unwrap();
    assert_eq!(
        (
            s.timezone.as_str(),
            s.default_lead_min,
            s.all_day_time.as_str()
        ),
        ("UTC", 15, "09:00")
    );
}
#[tokio::test]
async fn transaction_rolls_back_on_error() {
    let h = TestHub::new();
    let r: Result<(), CoreError> = h.db.transaction(|tx| {
        tx.exec("INSERT INTO settings(key,value) VALUES('x','1')", &[])
            .unwrap();
        Err(CoreError::Internal("boom".into()))
    });
    assert!(r.is_err());
    assert!(h.sql("SELECT * FROM settings WHERE key='x'").is_empty());
}
#[tokio::test]
async fn health_route() {
    let r = TestHub::new()
        .request("GET", "/api/health", None, &[])
        .await;
    assert_eq!((r.status, r.json()), (200, serde_json::json!({"ok": true})));
}
#[tokio::test]
async fn unknown_route_is_404_json() {
    let r = TestHub::new().request("GET", "/api/nope", None, &[]).await;
    assert_eq!(r.status, 404);
    assert_eq!(r.json()["error"], "not_found");
}
