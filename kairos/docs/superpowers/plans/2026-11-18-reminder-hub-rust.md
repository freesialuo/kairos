# Kairos (all-Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status (2026-11-18).** Tasks 1–17 are implemented and verified locally. The final run passed 185 native tests, 55 core WebAssembly tests under Node, native and WebAssembly Clippy, the Wrangler adapter lifecycle, and the real Chrome passkey and reminder journey. The production Worker deployment dry run passed. Setup and operation are documented in [the README](../../../README.md). Real Cloudflare startup time, largest-feed CPU and delivery to the owner's Bark device require the production origin and account.

The checkboxes below record completed implementation, test coverage and final checks. An unchecked historical red-run step means that its original failing run was not independently recorded. For combined red/green instructions, the implementation and passing run are covered by the final suites.

**Goal:** Build the single-user reminder hub specified in the design (ICS feeds + API + manual events → Bark reminders from a self-arming Durable Object alarm, passkey-protected web UI) entirely in Rust on Cloudflare Workers.

**Architecture:** A Cargo workspace. `kairos-core` holds every behaviour (domain, scheduling, sync, auth, WebAuthn verification, HTTP routing) behind ports for DB, clock, entropy, HTTP, alarm and commit, so it is tested natively on `rusqlite` through `kairos-testkit`. `kairos-worker` is a thin `workers-rs` layer: an entry Worker that forwards `/api/*` and `/auth/*` to one SQLite-backed Durable Object `Hub`, plus adapters that implement the ports. `kairos-web` is a Leptos client-side app served as static assets; `kairos-types` holds the DTOs shared by all of them.

**Tech Stack:** Rust (stable, plus a pinned nightly for `worker-build --panic-unwind`), `worker` (workers-rs), `wasm-bindgen`, `matchit`, `serde`/`serde_json`, `ical`, `rrule`, `jiff`, `sha2`/`hmac`/`subtle`, `getrandom`, `ciborium`/`coset`, `p256`/`ed25519-dalek`/`rsa`, `base64`, Leptos + `leptos_router` + `gloo-net` + Trunk, `rusqlite` (bundled), `proptest`, `wasm-bindgen-test`, `chromiumoxide`, `tokio`/`axum` (test servers only), Wrangler.

**Spec:** [`docs/2026-11-18-rust-architecture.md`](../../2026-11-18-rust-architecture.md) — read it before starting any task; section numbers below (§n) refer to it. It supersedes `docs/2026-11-17-design.md` and the TypeScript plan `2026-11-17-reminder-hub.md`.

## Global Constraints

- **Paths** are relative to `kairos/` (the git repository root is its parent, `CodeSpace/`).
- **Safe file disposal:** never delete with `rm`, `rmdir`, `unlink`, `git clean`, `find -delete`, or programmatic unlink (`std::fs::remove_*` included). Move removed files to Trash (`trash <path>` or `mv <path> ~/.Trash/`).
- **All application code is Rust.** The only hand-written non-Rust files are `crates/kairos-web/boot.js` (exact content in Task 15), `crates/kairos-web/_headers`, `crates/kairos-web/index.html`, `crates/kairos-web/style.css`, `wrangler.toml`, and Cargo/Trunk/toolchain config.
- **Free plan:** ≤ 10 ms CPU and ≤ 50 subrequests per invocation; no KV, no D1; exactly one Cron Trigger (`0 */6 * * *`, the watchdog, §7.8).
- **Singleton:** exactly one Durable Object instance, `HUB` → `id_from_name("main")`, class `Hub`, migration `v1` `new_sqlite_classes = ["Hub"]`.
- **No wall clock, no panics in production code:** time only from the `Clock` port; `clippy::unwrap_used`, `expect_used`, `panic`, `indexing_slicing` denied outside tests; `clippy::disallowed_methods` lists `std::time::SystemTime::now`, `std::time::Instant::now`, `jiff::Timestamp::now`, `chrono::Utc::now`, `chrono::Local::now` (configured in `clippy.toml`, Task 1). `inner_html` is banned in `kairos-web` (Task 15 adds the check).
- **Release profile:** `lto = true`, `codegen-units = 1`, `strip = true`, `overflow-checks = true`, `opt-level = "z"` for `kairos-web` only.
- **Time representation:** instants are UTC epoch milliseconds (`i64`); wall-clock strings are `YYYY-MM-DDTHH:mm:ss` (timed) or `YYYY-MM-DD` (all-day).
- **Spec constants (do not alter):**
  - reminder window **60 days**; `deliveries` and finished `reminders` retention **90 days**;
  - default lead **15 min**; all-day reminder time **`09:00`**; week starts **Monday**;
  - sync interval default **30 min**, minimum **15 min**; feed cap **5 MB** (`5 * 1024 * 1024`); alert after **3** consecutive failures;
  - send attempts: **1 initial + 3 retries**, delays **1, 5, 15 min**; late threshold **2 min**; "already started" grace **2 min** (timed);
  - dispatch batch **20**; queue batch **10**; queue item dropped after **3** failed attempts, backoff `2^attempts` min;
  - expansion budget **2,000** raw slots; anchor lag **1 day**; calendar/API ranges ≤ **92 days**;
  - normal alarm floor `now + 1 s`; error floor `now + min(2^(streak − 1), 15)` min;
  - recovery: **5** wrong attempts/hour → **1 h** lockout; challenge cookie TTL **2 min**; session sliding TTL **30 days**, extended at most once per day;
  - API: `title` 1–200; `location`/`description` ≤ 2,000; `reminders` ≤ 5 values in 0–10,080; `exdates` ≤ 200; `external_id` `^[A-Za-z0-9._:-]{1,200}$`.
- **Configuration:** vars `RP_ID`, `ORIGIN`; secrets `RECOVERY_CODE`, `SESSION_SECRET`.
- **Every state-changing HTTP route ends with `hub.rearm(Floor::Http)`**; `run_alarm` always re-arms (`Floor::AlarmClean` or `Floor::AlarmError`).
- **Cursor rule:** `Db::exec` returns materialised rows; never hold SQL state across an `await`. Transactions (`Db::transaction`) contain no `await` (enforced by the synchronous closure).
- **Untrusted text** (feed and event fields) is rendered as text only; CSP per §13.2.
- **UI copy** in English, in `crates/kairos-web/src/strings.rs`.
- **Dependencies:** add with `cargo add` (latest release at the time of the task); never below these floors: `worker` 0.8.7, `rrule` 0.14, `ical` 0.11, `jiff` 0.2, `matchit` 0.8, `serde_json` 1, `getrandom` 0.3, `base64` 0.22, `ciborium` 0.2. Commit `Cargo.lock`.
- **Commits:** one per task (more is fine), Conventional Commits, no attribution lines.

## Review Focus

1. **Real-world feed bytes:** a feed with CRLF line endings, a UTF-8 BOM, and a folded line that splits a multi-byte UTF-8 character (e.g. `会议` folded mid-character) parses to the right title, and invalid UTF-8 is a sync failure, not a panic. Test added to Task 4.
2. **API time with an explicit offset:** `PUT /api/v1/events/x` with `"start": "2026-11-20T09:00:00+08:00"` and no `timezone`, while the user zone is `Europe/Berlin`, is stored as Berlin wall time `2026-11-20T02:00:00` with `tzid = "Europe/Berlin"`, and its reminder fires at the same instant after the user switches to `Asia/Tokyo`. Test added to Task 13.
3. **Multi-day all-day event:** a 3-day all-day event gets exactly one reminder (09:00 on the first day), and that reminder is still sent late on day 2 after an outage but skipped on day 4 (§7.6 "already started"). Test added to Task 9.
4. **Calendar display across zones:** a weekly `09:00 America/New_York` event shown to a user in `Asia/Shanghai` has `start_local` `21:00` before the US change back to standard time (2026-11-01) and `22:00` after it, in the same response. Test added to Task 14.
5. **Unbounded feed without `Content-Length`:** a chunked 6 MB response is aborted at 5 MB by the real adapter (not only the fake) and counts as a sync failure. Test added to Task 2 (adapter) and Task 8 (core).

---

## File Structure

```
kairos/
  Cargo.toml                 workspace (members below), shared [profile.release], [workspace.lints]
  rust-toolchain.toml        stable channel + wasm32-unknown-unknown target
  clippy.toml                disallowed-methods list
  .cargo/config.toml         wasm32 rustflags for getrandom (if the resolved version needs them)
  wrangler.toml              entry Worker, DO binding, assets, cron, [env.e2e]
  README.md                  setup, deploy, operations
  docs/rust-on-workers-notes.md   experiment log (§20)
  xtask/src/main.rs          cargo xtask build | test | e2e | deploy
  crates/
    kairos-types/src/
      lib.rs  api_v1.rs  admin.rs  auth.rs  repeat.rs  limits.rs
    kairos-core/src/
      lib.rs                 Hub<P>, re-exports
      ports.rs               Value, Row, Db, Clock, Entropy, Http, Alarm, Commit, Ports, OutRequest/OutResponse
      error.rs               CoreError, DbError, NetError
      config.rs              Config
      db/schema.rs           MIGRATIONS, migrate
      db/settings.rs         Settings, get/put settings, meta
      http/mod.rs            Request, Response, Method, helpers
      http/router.rs         Hub::handle, route tables (matchit)
      http/auth_routes.rs    /auth/*
      http/admin/*.rs        /api/admin/* (calendar, sources, events, settings, channel, passkeys, tokens, log, queue, export)
      http/api_v1.rs         /api/v1/*
      http/validate.rs       payload → EventRow
      http/internal.rs       /internal/rearm, /__test/* (feature test-hooks)
      time/zone.rs           wall_to_utc, utc_to_wall, parse/format wall values
      ics/parse.rs           ICS text → ParsedFeed
      ics/expand.rs          expand_series
      calendar/model.rs      EventRow, StoredEvent, Occurrence, Anchor, ExpandStatus
      calendar/events.rs     event store
      calendar/plan.rs       resolve_leads, plan_reminders
      calendar/reconcile.rs  reconcile_series
      sched/queue.rs         reconcile queue
      sched/dispatch.rs      dispatch_due
      sched/alarm.rs         run_alarm, run_step, compute_next_alarm, rearm, maintenance, watchdog
      sources/model.rs       SourceRow + store
      sources/sync.rs        sync_due_source, send_source_alerts
      notify/message.rs      Message, format_reminder, format_source_alert
      notify/bark.rs         BarkConfig, send_bark, load_channel
      auth/crypto.rs  auth/cookies.rs  auth/session.rs  auth/csrf.rs  auth/recovery.rs  auth/challenge.rs  auth/tokens.rs
      webauthn/mod.rs  webauthn/authdata.rs  webauthn/cose.rs  webauthn/verify.rs  webauthn/options.rs
      export.rs              export JSON / ICS
    kairos-testkit/
      src/lib.rs             TestHub, TestPorts, FakeClock, FakeHttp, FakeAlarm, SeededEntropy, SqliteDb, fixtures helpers
      src/authenticator.rs   software WebAuthn authenticator (ES256, EdDSA, RS256)
      tests/*.rs             all Hub integration tests (one file per task area)
      fixtures/*.ics
    kairos-worker/src/
      lib.rs                 #[event(fetch)], #[event(scheduled)]
      hub_do.rs              #[durable_object] Hub
      convert.rs             worker::Request/Response ↔ kairos_core::http
      adapters/{db.rs, raw_storage.rs, clock.rs, entropy.rs, http.rs, alarm.rs, commit.rs, mod.rs}
    kairos-web/
      index.html  boot.js  _headers  style.css  Trunk.toml
      src/{main.rs, app.rs, api.rs, passkey.rs, b64.rs, strings.rs, calendar_grid.rs, pages/*.rs, components/*.rs}
    e2e/
      src/lib.rs             WranglerDev harness, MockServer (Bark + feeds), Browser helper
      tests/adapters.rs      real-DO adapter smoke tests
      tests/smoke.rs         browser end-to-end
```

**Why `kairos-testkit` is a separate crate:** integration tests need `rusqlite` and fakes that implement `kairos-core`'s ports. Putting them in `kairos-core/tests` would need a dev-dependency cycle (two copies of `kairos-core` types). `kairos-testkit` depends on `kairos-core`, and the integration tests live in `kairos-testkit/tests/`. Pure unit tests stay inside `kairos-core` modules (`#[cfg(test)] mod tests`).

**Pure-module tests on wasm32:** each `#[cfg(test)] mod tests` in `kairos-core/src/{time,ics,calendar/plan.rs,notify/message.rs,webauthn,auth/crypto.rs}` starts with `#[cfg(target_arch = "wasm32")] use wasm_bindgen_test::wasm_bindgen_test as test;` so `cargo xtask test` can also run them under `wasm-bindgen-test-runner` (Node).

### Shared core types (created in Task 1 unless noted; every task uses these names)

```rust
// ports.rs
pub enum Value { Null, Integer(i64), Real(f64), Text(String), Blob(Vec<u8>) }
pub struct Row { /* column name → Value */ }
impl Row {
    pub fn i64(&self, col: &str) -> Result<i64, DbError>;
    pub fn opt_i64(&self, col: &str) -> Result<Option<i64>, DbError>;
    pub fn text(&self, col: &str) -> Result<String, DbError>;
    pub fn opt_text(&self, col: &str) -> Result<Option<String>, DbError>;
    pub fn blob(&self, col: &str) -> Result<Vec<u8>, DbError>;
    pub fn bool(&self, col: &str) -> Result<bool, DbError>;
}
pub trait Db {
    fn exec(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, DbError>;
    fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T, CoreError>) -> Result<T, CoreError>;
}
pub trait Clock { fn now_ms(&self) -> i64; }
pub trait Entropy { fn fill(&self, buf: &mut [u8]); }
pub struct OutRequest { pub method: String, pub url: String, pub headers: Vec<(String, String)>, pub body: Option<Vec<u8>>, pub max_body_bytes: usize }
pub struct OutResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub trait Http { async fn send(&self, req: OutRequest) -> Result<OutResponse, NetError>; }
pub trait Alarm { async fn get(&self) -> Option<i64>; async fn set(&self, at_ms: i64); }
pub trait Commit { async fn commit_point(&self); }
pub trait Ports { type Db: Db; type Clock: Clock; type Entropy: Entropy; type Http: Http; type Alarm: Alarm; type Commit: Commit; }

// error.rs
pub struct DbError(pub String);
pub enum NetError { Network(String), TooLarge }
pub enum CoreError {
    Db(DbError), Net(NetError),
    Invalid { field: &'static str, message: String },   // → 400 { error, field }
    Unauthorized, Forbidden, NotFound, TooManyRequests,
    Conflict { code: &'static str, message: String },   // → 409 { error: code, warning: message }
    Internal(String),
}

// config.rs
pub struct Config {
    pub rp_id: String, pub origin: String,
    pub recovery_digest: [u8; 32], pub session_secret: Vec<u8>,
    pub allow_http_urls: bool,      // true only in test-hooks builds
}

// lib.rs
pub struct Hub<P: Ports> {
    pub db: P::Db, pub clock: P::Clock, pub entropy: P::Entropy,
    pub http: P::Http, pub alarm: P::Alarm, pub commit: P::Commit, pub config: Config,
}
impl<P: Ports> Hub<P> {
    pub fn now(&self) -> i64;                              // clock + test offset (Task 17)
    pub fn migrate(&self) -> Result<(), CoreError>;
    pub async fn handle(&self, req: http::Request) -> http::Response;     // never panics outward (Task 1)
    pub async fn run_alarm(&self) -> sched::alarm::AlarmReport;          // Task 9 (Task 1: re-arm only)
    pub async fn rearm(&self, floor: sched::alarm::Floor);               // Task 9 (Task 1: stub no-op)
}

// http/mod.rs
pub enum Method { Get, Post, Put, Patch, Delete, Other }
pub struct Request { pub method: Method, pub path: String, pub query: Vec<(String, String)>, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
pub struct Response { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
impl Response {
    pub fn json<T: serde::Serialize>(status: u16, body: &T) -> Response;
    pub fn empty(status: u16) -> Response;
    pub fn from_error(e: &CoreError) -> Response;   // 400/401/403/404/409/429/500, JSON { error, field? , warning? }
}

// calendar/model.rs (Task 3–5 fill these in; declared in Task 1 with all fields)
pub enum Origin { Ics, Api, Manual }
pub struct EventRow {
    pub origin: Origin, pub source_id: Option<i64>, pub series_key: String,
    pub uid: Option<String>, pub recurrence_id: Option<String>, pub external_id: Option<String>,
    pub title: String, pub description: Option<String>, pub location: Option<String>,
    pub all_day: bool, pub start_local: String, pub tzid: Option<String>, pub duration_sec: i64,
    pub rrule: Option<String>, pub exdates: Vec<String>,
    pub lead_minutes: Option<Vec<u32>>,   // None = inherit; Some(vec![]) = no reminders
    pub valarm_minutes: Vec<u32>,
}
impl EventRow { pub fn content_hash(&self) -> String; }   // SHA-256 hex of the canonical JSON of every field above
pub enum ExpandStatus { Complete, CatchingUp, TooDense }   // stored as 'complete' | 'catching_up' | 'too_dense'
pub struct Anchor { pub slot: String, pub index: u32, pub hash: String }   // stored as JSON
pub struct StoredEvent {
    pub id: i64, pub row: EventRow, pub hash: String, pub updated_at: i64,
    pub expand_anchor: Option<Anchor>, pub expand_status: ExpandStatus, pub expand_warning: Option<String>,
}
pub struct Occurrence {
    pub event_id: i64, pub series_key: String, pub occ_key: String, pub slot: String,
    pub start_utc: i64, pub end_utc: i64, pub all_day: bool,
}
```

Identity keys (§6.1): `series_key` = `ics:<sourceId>:<uid>` | `api:<externalId>` | `manual:<eventId>`; `occ_key` = `<series_key>|<slot>` (`slot` = original wall-clock start, or `single`); reminder `key` = `<occ_key>|<leadMin>` or `<occ_key>|allday`.

### Test harness (`kairos-testkit`, Task 1; extended where noted)

```rust
pub const NOW: &str = "2026-11-20T08:00:00Z";
pub fn ms(iso: &str) -> i64;                       // RFC 3339 → epoch ms (via jiff)
pub struct TestHub { pub hub: Hub<TestPorts> }     // Deref to Hub
impl TestHub {
    pub fn new() -> TestHub;                       // in-memory rusqlite, migrated, clock at NOW, origin "https://kairos.test", rp_id "kairos.test", recovery code "correct horse"
    pub fn set_now(&self, iso: &str);  pub fn advance_min(&self, minutes: i64);
    pub fn http(&self) -> &FakeHttp;               // script responses by URL prefix; records requests
    pub fn alarm_at(&self) -> Option<i64>;  pub fn set_alarm_calls(&self) -> usize;
    pub fn sql(&self, q: &str) -> Vec<Row>;        // test-only raw query
    pub async fn request(&self, method: &str, path: &str, body: Option<serde_json::Value>, cookies: &[&str]) -> TestResponse;  // adds Origin: https://kairos.test
    pub async fn admin_session(&self) -> String;   // Task 12: full session cookie without a browser (registers a key via the software authenticator)
}
pub struct FakeHttp;  impl FakeHttp {
    pub fn respond(&self, url_prefix: &str, status: u16, headers: &[(&str, &str)], body: &[u8]);
    pub fn fail(&self, url_prefix: &str, err: NetError);
    pub fn requests(&self) -> Vec<OutRequest>;
}
// FakeHttp honours OutRequest::max_body_bytes: a scripted body longer than the cap returns Err(NetError::TooLarge).
// Fault injection (created in Task 1; used from Task 6 on): `h.faults()` →
//   fail_sql_matching(substr)  – the matching statement returns DbError
//   panic_sql_matching(substr) – the matching statement panics
//   kill_after_sql_matching(substr) – after the matching statement runs, the next Http::send or Commit::commit_point
//       never resolves; the test polls run_alarm()/the step once with `now_or_never()` and drops it. This models an
//       invocation killed by the platform at its next commit point (writes before it persist, nothing after runs).
```

---

### Task 1: Workspace, ports, schema, and the core router

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `.gitignore` (`target/`, `crates/kairos-web/dist/`, `build/`, `.wrangler/`), `crates/kairos-types/src/lib.rs`, `crates/kairos-core/src/{lib.rs, ports.rs, error.rs, config.rs, db/schema.rs, db/settings.rs, http/mod.rs, http/router.rs, calendar/model.rs, sched/alarm.rs}`, `crates/kairos-testkit/src/lib.rs`, `crates/kairos-testkit/tests/foundation.rs`, `xtask/src/main.rs`
- Test: `crates/kairos-testkit/tests/foundation.rs`

**Interfaces:**
- Produces: everything in "Shared core types" and "Test harness" above; `MIGRATIONS: &[&str]` (migration 1 = **exactly** the DDL in §6.2, including `sources.alerted`, `sources.notify`, `passkeys.alg`); `Settings { timezone: String, default_lead_min: u32, all_day_time: String }` with defaults `UTC`, `15`, `09:00`; `get_settings(db) / put_settings(db, &Settings)`; `get_meta(db, key) -> Option<String>` / `set_meta(db, key, value)` / `del_meta(db, key)` (keys stored as `meta:<key>`); in `sched/alarm.rs`: `Floor { Http, AlarmClean, AlarmError }` (`Http`: normal floor, error streak untouched; `AlarmClean`: normal floor, streak reset to 0; `AlarmError`: streak + 1, error floor), `AlarmReport { step_errors: Vec<(String, String)>, top_level_error: Option<String> }`, `StepOutcome { ok: u32, failed: u32, step_error: Option<String> }`; `xtask test` = `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`.

- [x] **Step 1: Write the failing tests** in `crates/kairos-testkit/tests/foundation.rs`

```rust
#[tokio::test] async fn migration_creates_all_tables_and_is_idempotent() {
    let h = TestHub::new();
    h.migrate().unwrap();                                   // second run is a no-op
    let names: Vec<String> = h.sql("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .iter().map(|r| r.text("name").unwrap()).collect();
    assert_eq!(names, ["api_tokens","auth_failures","channels","deliveries","events","passkeys",
        "reconcile_queue","reminders","sessions","settings","sources"]);
    assert_eq!(get_meta(&h.db, "schema_version").as_deref(), Some("1"));
}
#[tokio::test] async fn settings_have_spec_defaults() {
    let s = get_settings(&TestHub::new().db).unwrap();
    assert_eq!((s.timezone.as_str(), s.default_lead_min, s.all_day_time.as_str()), ("UTC", 15, "09:00"));
}
#[tokio::test] async fn transaction_rolls_back_on_error() {
    let h = TestHub::new();
    let r: Result<(), CoreError> = h.db.transaction(|tx| {
        tx.exec("INSERT INTO settings(key,value) VALUES('x','1')", &[]).unwrap();
        Err(CoreError::Internal("boom".into()))
    });
    assert!(r.is_err());
    assert!(h.sql("SELECT * FROM settings WHERE key='x'").is_empty());
}
#[tokio::test] async fn health_route() {
    let r = TestHub::new().request("GET", "/api/health", None, &[]).await;
    assert_eq!((r.status, r.json()), (200, serde_json::json!({"ok": true})));
}
#[tokio::test] async fn unknown_route_is_404_json() {
    let r = TestHub::new().request("GET", "/api/nope", None, &[]).await;
    assert_eq!(r.status, 404); assert_eq!(r.json()["error"], "not_found");
}
```

- [x] **Step 2: Run** `cargo test -p kairos-testkit --test foundation` — Expected: FAIL (crates do not exist / do not compile).
- [x] **Step 3: Implement** the workspace and the listed modules.
  - `Hub::handle` wraps routing in `AssertUnwindSafe(..).catch_unwind()` (`futures::FutureExt`); a panic becomes `500 {"error":"internal"}` and is logged.
  - `migrate` reads `meta:schema_version` (absent = 0) and applies `MIGRATIONS[v..]` in order, each inside `Db::transaction`, then writes the new version.
  - `rearm` and `run_alarm` are stubs in this task (`rearm` does nothing; `run_alarm` returns an empty report); Task 9 replaces them.
  - `SqliteDb` (testkit) implements `Db::transaction` with a real `BEGIN`/`COMMIT`/`ROLLBACK`; nested calls are not needed and return `CoreError::Internal`.
- [x] **Step 4: Run** `cargo xtask test` — Expected: PASS, clippy clean.
- [x] **Step 5: Commit** — `feat: workspace, core ports, schema migration 1 and router`

---

### Task 2: Worker adapters and the platform spike (verify-early items, §20)

**Files:**
- Create: `wrangler.toml`, `crates/kairos-worker/src/{lib.rs, hub_do.rs, convert.rs, adapters/*.rs}`, `crates/kairos-core/src/http/internal.rs` (test-hooks part), `crates/e2e/src/lib.rs`, `crates/e2e/tests/adapters.rs`, `docs/rust-on-workers-notes.md`, `.cargo/config.toml` (only if needed)
- Modify: `xtask/src/main.rs` (`e2e` subcommand: ensure `crates/kairos-web/dist/` exists with a placeholder `index.html` if the web app is not built yet, start the harness, run `cargo test -p e2e -- --test-threads=1`)

**Interfaces:**
- Consumes: Task 1 ports, `Hub::{migrate, handle, run_alarm, rearm}`.
- Produces:
  - `WorkerPorts` implementing `Ports` with these adapters: `DoDb` (SqlStorage + `transactionSync`), `DateClock`, `CryptoEntropy`, `FetchHttp` (streams the body and returns `NetError::TooLarge` past `max_body_bytes`), `DoAlarm`, `DoCommit` (`ctx.storage.sync()`).
  - Feature `test-hooks` on `kairos-worker` and `kairos-core` (sets `Config::allow_http_urls = true`; enables `/__test/*`).
  - `/__test/adapter/tx-rollback` (POST → `{ rolled_back: bool }`), `/__test/adapter/commit` (POST → `{ ok: true }` after `commit_point().await`), `/__test/adapter/alarm` (POST `{ at }` → sets it; GET → `{ at }`), `/__test/adapter/fetch` (POST `{ url, max }` → `{ status, len }` or `{ error: "too_large" }`).
  - e2e harness: `WranglerDev::start() -> &'static WranglerDev` (one process shared by all tests in the binary; runs `npx wrangler dev --env e2e --port 8799 --local`, waits for `GET /api/health`), `MockServer::start() -> MockServer` (axum on 127.0.0.1, routes `/bark/push`, `/feeds/:name`, `/big/:mb` chunked without Content-Length), `MockServer::url(path)`.

`wrangler.toml` (exact keys):
```toml
name = "kairos"
main = "build/worker/shim.mjs"
compatibility_date = "2026-11-01"
[build]
command = "cargo install -q worker-build && worker-build --release --panic-unwind crates/kairos-worker"
[assets]
directory = "crates/kairos-web/dist"
not_found_handling = "single-page-application"
run_worker_first = ["/api/*", "/auth/*"]
[[durable_objects.bindings]]
name = "HUB"
class_name = "Hub"
[[migrations]]
tag = "v1"
new_sqlite_classes = ["Hub"]
[triggers]
crons = ["0 */6 * * *"]
[vars]
RP_ID = "CHANGE-ME"
ORIGIN = "https://CHANGE-ME"
[env.e2e]
build = { command = "cargo install -q worker-build && worker-build --dev --panic-unwind crates/kairos-worker --features test-hooks" }
assets = { directory = "crates/kairos-web/dist", not_found_handling = "single-page-application", run_worker_first = ["/api/*", "/auth/*", "/__test/*"] }
vars = { RP_ID = "localhost", ORIGIN = "http://localhost:8799" }
# durable_objects / migrations / triggers repeated under env.e2e as wrangler requires
```
(Adjust the `worker-build` invocation if the resolved version takes the crate path differently; record it in the notes. The e2e secrets come from `.dev.vars.e2e`: `RECOVERY_CODE=correct horse`, `SESSION_SECRET=<64 hex>`; add that file to `.gitignore`.)

The `transactionSync` binding is the riskiest piece; the approach:
```rust
#[wasm_bindgen]
extern "C" {
    pub type RawStorage;   // the JS DurableObjectStorage object
    #[wasm_bindgen(method, catch, js_name = transactionSync)]
    fn transaction_sync(this: &RawStorage, f: &js_sys::Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, js_name = sync)]
    fn sync(this: &RawStorage) -> js_sys::Promise;
}
```
`DoDb::transaction` stores the closure's `Result<T, CoreError>` in a `RefCell<Option<…>>`, wraps the call in a `Closure` that returns `Err(JsValue)` (which throws, so the runtime rolls back) when the Rust result is `Err`, and maps the thrown value back to the stored `CoreError`. Obtaining `RawStorage` from `worker::State`: first look for a public accessor in the resolved `worker` source; otherwise vendor a `[patch.crates-io]` copy of `worker` that adds `pub fn raw_storage(&self) -> JsValue` and record the patch in the notes (and as an upstream issue/PR draft).

Row conversion in `DoDb::exec`: iterate the cursor's JS row objects; `null` → `Null`, string → `Text`, `ArrayBuffer`/`Uint8Array` → `Blob`, number → `Integer` when it is an integer within ±2^53, else `Real`.

`Hub` DO: holds `CoreHub<WorkerPorts>` and a `migrated: Cell<bool>`; `fetch` and `alarm` call `migrate()` once first. `alarm()` calls `run_alarm` and always returns `Ok`. Entry `fetch` forwards any path starting with `/api/`, `/auth/` (and `/__test/` with the feature) to the stub unchanged; anything else → 404 (static assets never reach it). Entry `scheduled` is added in Task 9.

- [x] **Step 1: Write the failing adapter tests** in `crates/e2e/tests/adapters.rs`

```rust
#[tokio::test] async fn health_through_worker_and_do() { let w = WranglerDev::start().await; assert_eq!(w.get_json("/api/health").await["ok"], true); }
#[tokio::test] async fn transaction_sync_rolls_back_on_error() { let w = WranglerDev::start().await; assert_eq!(w.post_json("/__test/adapter/tx-rollback", json!({})).await["rolled_back"], true); }
#[tokio::test] async fn commit_point_returns() { /* POST /__test/adapter/commit → ok */ }
#[tokio::test] async fn alarm_set_and_get_round_trip() { /* set at = now+3_600_000 → GET returns it */ }
#[tokio::test] async fn fetch_aborts_chunked_body_past_cap() {   // Review Focus 5 (adapter half)
    let (w, m) = (WranglerDev::start().await, MockServer::start().await);
    let r = w.post_json("/__test/adapter/fetch", json!({"url": m.url("/big/6"), "max": 5*1024*1024})).await;
    assert_eq!(r["error"], "too_large");
}
#[tokio::test] async fn migration_runs_on_real_do_sqlite() { /* GET /api/health twice; restart wrangler; health still ok (schema_version persisted) */ }
```

- [ ] **Step 2: Run** `cargo xtask e2e` — Expected: FAIL (worker crate missing).
- [x] **Step 3: Implement** `kairos-worker`, the adapters, the test-hook routes, and the harness as specified.
- [x] **Step 4: Run** `cargo xtask e2e` — Expected: PASS. Also run `npx wrangler deploy --dry-run --outdir build/dry` and record in `docs/rust-on-workers-notes.md`: Wasm size, how `RawStorage` was obtained, the `worker-build` command that worked, whether `getrandom` needed the rustflag, and any `--panic-unwind` issue.
- [x] **Step 5: Commit** — `feat: workers-rs adapters, transactionSync binding and adapter smoke tests`

---

### Task 3: Time zones

**Files:**
- Create: `crates/kairos-core/src/time/zone.rs` (+ `time/mod.rs`)
- Test: unit tests in the same file; property tests in `crates/kairos-testkit/tests/time_props.rs`

**Interfaces:**
- Produces:
```rust
pub enum WallValue { Timed(jiff::civil::DateTime), Date(jiff::civil::Date) }
pub fn parse_wall(s: &str) -> Result<WallValue, TimeError>;          // 'YYYY-MM-DDTHH:mm:ss' | 'YYYY-MM-DD'
pub fn format_wall(v: &WallValue) -> String;
pub fn is_valid_zone(name: &str) -> bool;                            // IANA or "UTC"
pub fn wall_to_utc(wall: jiff::civil::DateTime, zone: &str) -> Result<i64, TimeError>;   // Disambiguation::Compatible
pub fn utc_to_wall(ms: i64, zone: &str) -> Result<jiff::civil::DateTime, TimeError>;
pub fn parse_api_instant(s: &str, zone_hint: &str) -> Result<(String /*wall*/, String /*zone*/), TimeError>; // §8 "explicit offset"
pub enum TimeError { BadFormat(String), UnknownZone(String), OutOfRange }
```

- [x] **Step 1: Write the failing tests**

```rust
#[test] fn ny_gap_moves_forward() { assert_eq!(wall_to_utc(dt("2026-03-08T02:30:00"), "America/New_York").unwrap(), ms("2026-03-08T07:30:00Z")); }
#[test] fn berlin_gap_moves_forward() { assert_eq!(wall_to_utc(dt("2026-03-29T02:30:00"), "Europe/Berlin").unwrap(), ms("2026-03-29T01:30:00Z")); }
#[test] fn ny_overlap_takes_earlier() { assert_eq!(wall_to_utc(dt("2026-11-01T01:30:00"), "America/New_York").unwrap(), ms("2026-11-01T05:30:00Z")); }
#[test] fn berlin_overlap_takes_earlier() { assert_eq!(wall_to_utc(dt("2026-10-25T02:30:00"), "Europe/Berlin").unwrap(), ms("2026-10-25T00:30:00Z")); }
#[test] fn unknown_zone_is_error() { assert!(matches!(wall_to_utc(dt("2026-01-01T00:00:00"), "China Standard Time"), Err(TimeError::UnknownZone(_)))); }
#[test] fn api_offset_becomes_wall_in_hint_zone() {
    assert_eq!(parse_api_instant("2026-11-20T09:00:00+08:00", "Europe/Berlin").unwrap(),
               ("2026-11-20T02:00:00".to_string(), "Europe/Berlin".to_string()));
}
#[test] fn api_without_offset_keeps_wall() { assert_eq!(parse_api_instant("2026-11-20T09:00:00", "Asia/Tokyo").unwrap().0, "2026-11-20T09:00:00"); }
```
Property tests (`proptest`, zones `America/New_York`, `Europe/Berlin`, `Australia/Lord_Howe`, `Asia/Kolkata`, `UTC`; wall times in 2025–2027): (a) `utc_to_wall(wall_to_utc(w)) == w` whenever `w` exists in the zone; (b) for a gap wall time, `utc_to_wall(wall_to_utc(w)) == w + gap length`.

- [ ] **Step 2: Run** `cargo test -p kairos-core time:: && cargo test -p kairos-testkit --test time_props` — FAIL.
- [x] **Step 3: Implement** with `jiff` (`civil::DateTime::to_zoned` uses `Compatible` by default; look zones up with `jiff::tz::db().get`). `parse_api_instant`: if the string has an offset or `Z`, convert the instant to wall time in `zone_hint`; otherwise return the wall unchanged.
- [x] **Step 4: Run** both — PASS.
- [x] **Step 5: Commit** — `feat: wall-clock to UTC conversion with RFC 5545 DST rules`

---

### Task 4: ICS parsing

**Files:**
- Create: `crates/kairos-core/src/ics/parse.rs` (+ `ics/mod.rs`), `crates/kairos-testkit/fixtures/*.ics`
- Test: unit tests in `ics/parse.rs` (fixtures loaded with `include_str!("../../../kairos-testkit/fixtures/…")`)

**Interfaces:**
- Consumes: `parse_wall`, `is_valid_zone` (Task 3); `EventRow` (Task 1).
- Produces:
```rust
pub struct ParsedFeed { pub rows: Vec<EventRow>, pub warnings: Vec<String> }
pub enum IcsError { NotCalendar, InvalidUtf8 }
pub fn parse_ics(bytes: &[u8], source_id: i64) -> Result<ParsedFeed, IcsError>;
```

Rules (§9, implement each; one fixture per rule):
- Input: strip a UTF-8 BOM; decode strictly (invalid UTF-8 → `InvalidUtf8`); accept CRLF and LF; unfold *before* decoding text values so a fold inside a multi-byte character is rejoined. Use `ical::IcalParser` over `std::io::Cursor`; if its unfolding works on `&str` lines, unfold bytes yourself first.
- No root `VCALENDAR` → `NotCalendar`.
- `DTSTART`: `TZID` param valid → `tzid = Some(zone)`; `Z` suffix → `Some("UTC")`; neither → `None` (floating); invalid `TZID` → floating + warning `Unknown TZID "<id>" on "<summary>"; using your time zone`.
- `VALUE=DATE` → `all_day = true`, `start_local = YYYY-MM-DD`, default duration 86,400 s.
- Duration from `DTEND − DTSTART` (in wall time) or `DURATION`; default 0 for timed.
- `RRULE` value stored without the `RRULE:` prefix; `EXDATE` values (multi-valued and repeated) converted to wall strings in the row's zone.
- `RECURRENCE-ID` → override row (`recurrence_id` = wall string); `RANGE=THISANDFUTURE` treated the same.
- `STATUS:CANCELLED` on an override → its `recurrence_id` appended to the parent's `exdates`, no row; on the master → master and all its overrides dropped; on an orphan override → ignored.
- Override whose master is absent → kept.
- `VALARM` with relative `TRIGGER` ≤ 0 and `RELATED=START` (or default) → `valarm_minutes` (deduplicated, sorted); others ignored.
- `RDATE` → warning `RDATE ignored on "<summary>"`.
- `series_key = format!("ics:{source_id}:{uid}")`; `origin = Ics`; `lead_minutes = None`.

- [x] **Step 1: Write the failing tests** (one per rule; names below; each asserts the exact field values)

```rust
#[test] fn parses_timed_tzid_event() { /* tzid Some("Europe/Berlin"), start_local "2026-11-23T09:00:00", duration 3600 */ }
#[test] fn parses_utc_and_floating() {}
#[test] fn parses_all_day_default_duration() { /* all_day, "2026-12-25", 86400 */ }
#[test] fn unknown_tzid_falls_back_with_warning() { assert_eq!(f.warnings, [r#"Unknown TZID "China Standard Time" on "Standup"; using your time zone"#]); }
#[test] fn rrule_and_repeated_exdate() { /* exdates == ["2026-11-30T09:00:00","2026-12-07T09:00:00"] */ }
#[test] fn override_keeps_original_slot() {}
#[test] fn cancelled_instance_becomes_exdate() { /* 1 row; parent.exdates contains the cancelled slot */ }
#[test] fn cancelled_master_drops_series() { assert!(f.rows.is_empty()); }
#[test] fn orphan_cancelled_override_ignored() {}
#[test] fn orphan_override_kept() {}
#[test] fn thisandfuture_is_single_override() {}
#[test] fn valarm_relative_to_start_only() { /* TRIGGER:-PT10M,-PT1H, RELATED=END and absolute ignored → [10, 60] */ }
#[test] fn rdate_warns() { assert_eq!(f.warnings, [r#"RDATE ignored on "Dentist""#]); }
#[test] fn html_page_is_not_calendar() { assert!(matches!(parse_ics(b"<!doctype html><html>", 1), Err(IcsError::NotCalendar))); }
#[test] fn crlf_bom_and_folded_multibyte_title() {   // Review Focus 1
    let bytes = b"\xEF\xBB\xBFBEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261123T090000Z\r\nSUMMARY:\xE4\xBC\x9A\r\n \xE8\xAE\xAE\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert_eq!(parse_ics(bytes, 1).unwrap().rows[0].title, "会议");
}
#[test] fn invalid_utf8_is_error() { assert!(matches!(parse_ics(b"BEGIN:VCALENDAR\r\nSUMMARY:\xFF\r\nEND:VCALENDAR", 1), Err(IcsError::InvalidUtf8))); }
```
(`\xE4\xBC\x9A` is `会`; the fold splits `议` = `\xE8\xAE\xAE` onto the continuation line — adjust the split to land inside a character if the fixture above does not.)

- [ ] **Step 2: Run** `cargo test -p kairos-core ics::parse` — FAIL.
- [x] **Step 3: Implement** `parse_ics` per the rules.
- [x] **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: ICS parsing with cancellation, override and VALARM rules`

---

### Task 5: Recurrence expansion

**Files:**
- Create: `crates/kairos-core/src/ics/expand.rs`
- Test: unit tests in the same file; `crates/kairos-testkit/tests/expand_props.rs`

**Interfaces:**
- Consumes: `wall_to_utc`, `parse_wall` (Task 3); `StoredEvent`, `Occurrence`, `Anchor`, `ExpandStatus` (Task 1).
- Produces:
```rust
pub const EXPAND_BUDGET: u32 = 2_000;
pub const ANCHOR_LAG_MS: i64 = 86_400_000;
pub enum Budget { Slots(u32), Unlimited }                     // Unlimited also ignores the anchor
pub struct ExpandOpts { pub anchor_before_utc: Option<i64>, pub budget: Budget }   // Default: None, Slots(EXPAND_BUDGET)
pub struct ExpandResult { pub occurrences: Vec<Occurrence>, pub anchor: Option<Anchor>, pub status: ExpandStatus, pub warning: Option<String> }
pub fn expand_series(rows: &[StoredEvent], from_utc: i64, to_utc: i64, user_tz: &str, opts: &ExpandOpts) -> ExpandResult;
pub fn validate_rrule(rrule: &str) -> Result<(), String>;       // used by API/manual validation and the parser's warning path
```

Algorithm (§7.4) — the part the signature does not determine:
- Parent = the row without `recurrence_id` (may be absent: then only overrides). Zone = `tzid.unwrap_or(user_tz)`.
- Floating expansion: build `rrule::RRuleSet` from `"DTSTART:<wall as UTC basic format>\nRRULE:<rule>"` with **no EXDATEs**; for `UNTIL=…Z` on a zoned row, rewrite `UNTIL` first to the wall time of that instant in the row's zone (basic format + `Z`). Date-only starts use `T000000`.
- Anchor usable iff `budget != Unlimited` && `anchor.hash == parent.hash` && `wall_to_utc(anchor.slot) <= from_utc` && the rule has no `BYSETPOS`. Then rebuild with `DTSTART := anchor.slot` and `COUNT := COUNT − anchor.index` (if `COUNT` present); raw index starts at `anchor.index`.
- Iterate raw slots: `index += 1` per slot; budget counts raw slots; suppressed = `exdates ∪ overrides' recurrence_id`; convert with `wall_to_utc`; keep slots with `start >= from_utc && start < to_utc`.
- New anchor (if `anchor_before_utc` is `Some(t)`): the last raw slot with start `< t`, with its index and the parent hash; else unchanged.
- Budget exhausted before `from_utc` → `CatchingUp`, anchor = furthest slot reached, no occurrences. Exhausted inside the window → `TooDense`, occurrences so far, warning `Recurrence too dense; only the first N occurrences in the window are scheduled`.
- Overrides: one occurrence each, `slot = recurrence_id`, start from their own `start_local`/zone, kept if inside the window.
- A rule `rrule` rejects → treat the parent as non-recurring and set warning `Unsupported RRULE "<rule>": <reason>`.
- End = start + `duration_sec` (all-day: the wall date + N days, converted).
- Output sorted by `start_utc`, then `occ_key`.

- [x] **Step 1: Write the failing tests**

```rust
#[test] fn weekly_stays_at_nine_across_dst() { /* NY weekly 09:00 from 2026-10-26, window to 2026-11-16 → UTC hours 13,13,14,14 */ }
#[test] fn exdate_and_override_suppress_slots() {}
#[test] fn orphan_override_expands_alone() {}
#[test] fn count_includes_exdated_slots() { /* COUNT=5 with 1 EXDATE → 4 occurrences */ }
#[test] fn until_in_utc_on_zoned_rule() { /* Berlin daily 23:30, UNTIL=20261124T223000Z → last occurrence 2026-11-24 */ }
#[test] fn anchor_resumes_with_rebased_count() { /* anchored result == full result over the same window */ }
#[test] fn anchor_ignored_on_hash_mismatch() {}
#[test] fn bysetpos_ignores_anchor() {}
#[test] fn ancient_hourly_series_catches_up() {
    let r = expand_series(&[hourly_since("2010-01-01T00:00:00")], ms(NOW), ms(NOW) + 60*86_400_000, "UTC", &ExpandOpts::default());
    assert_eq!(r.status, ExpandStatus::CatchingUp);
    assert_eq!(r.anchor.unwrap().index, EXPAND_BUDGET - 1);
    assert!(r.occurrences.is_empty());
}
#[test] fn minutely_rule_is_too_dense() { /* status TooDense, occurrences.len() == EXPAND_BUDGET as usize, warning text exact */ }
#[test] fn unsupported_rule_is_non_recurring_with_warning() {}
#[test] fn rfc5545_examples() { /* table of the RFC 5545 §3.8.5.3 examples expressible without RDATE, each: rule, DTSTART (America/New_York), expected first N local starts */ }
```
`expand_props.rs`: differential property — generated parent rows (DAILY/WEEKLY with BYDAY and INTERVAL 1–3/MONTHLY BYMONTHDAY=31/YEARLY; optional COUNT ≤ 400, UNTIL, up to 3 EXDATEs; zones as in Task 3; DTSTART 2018–2026) → for windows starting at random offsets, expanding with an anchor computed by a previous call equals expanding with `Budget::Unlimited`.

- [ ] **Step 2: Run** `cargo test -p kairos-core ics::expand && cargo test -p kairos-testkit --test expand_props` — FAIL.
- [x] **Step 3: Implement** `expand_series` and `validate_rrule`.
- [x] **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: floating recurrence expansion with anchor and budget`

---

### Task 6: Event store, reminder planning, reconciliation, and the reconcile queue

**Files:**
- Create: `crates/kairos-core/src/calendar/{events.rs, plan.rs, reconcile.rs}`, `crates/kairos-core/src/sched/queue.rs`, `crates/kairos-core/src/sources/model.rs`
- Test: unit tests in `plan.rs`; `crates/kairos-testkit/tests/reconcile.rs`

**Interfaces:**
- Consumes: Tasks 1, 3, 5.
- Produces:
```rust
// sources/model.rs
pub struct SourceRow { pub id: i64, pub name: String, pub url: String, pub color: String, pub enabled: bool,
    pub interval_min: u32, pub default_lead_min: Option<u32>, pub honor_valarm: bool, pub notify: bool,
    pub etag: Option<String>, pub last_modified: Option<String>, pub last_sync_at: Option<i64>,
    pub last_result: Option<String>, pub last_error: Option<String>, pub consecutive_failures: u32, pub alerted: bool }
pub fn get_source(db: &impl Db, id: i64) -> Result<Option<SourceRow>, CoreError>;
pub fn list_sources(db: &impl Db) -> Result<Vec<SourceRow>, CoreError>;
// calendar/events.rs
pub struct UpsertOutcome { pub id: i64, pub created: bool, pub changed: bool }
pub fn upsert_event(db: &impl Db, row: &EventRow, now: i64) -> Result<UpsertOutcome, CoreError>;   // identity = (series_key, recurrence_id); changed content resets expand_anchor/status/warning
pub fn insert_manual_event(db: &impl Db, row: EventRow, now: i64) -> Result<i64, CoreError>;       // assigns series_key "manual:<id>"
pub fn load_series(db: &impl Db, series_key: &str) -> Result<Vec<StoredEvent>, CoreError>;
pub fn delete_series(db: &impl Db, series_key: &str, now: i64) -> Result<(), CoreError>;          // events + reconcile (withdraws reminders)
pub fn add_exdate(db: &impl Db, event_id: i64, slot: &str, now: i64) -> Result<(), CoreError>;
// calendar/plan.rs (pure)
pub enum Leads { Muted, Minutes(Vec<u32>) }
pub fn resolve_leads(row: &EventRow, source: Option<&SourceRow>, global_default: u32) -> Leads;
pub struct PlannedReminder { pub key: String, pub series_key: String, pub event_id: i64, pub occ_start_utc: i64, pub lead_min: Option<u32>, pub fire_at: i64 }
pub fn plan_reminders(occs: &[Occurrence], leads: &Leads, all_day_time: &str, zone_of: &dyn Fn(i64) -> String) -> Vec<PlannedReminder>;
// calendar/reconcile.rs
pub enum ReconcileOutcome { Done(ExpandStatus), Gone }
pub fn reconcile_series(db: &impl Db, series_key: &str, now: i64) -> Result<ReconcileOutcome, CoreError>;  // caller wraps in a transaction
// sched/queue.rs
pub const QUEUE_BATCH: usize = 10;
pub enum SeriesFilter { Source(i64), FloatingOrAllDay, RecurringOnly, All }
pub fn enqueue_series(db: &impl Db, series_key: &str, reason: &str, now: i64) -> Result<(), CoreError>;  // upsert: keeps earliest not_before, resets attempts
pub fn enqueue_where(db: &impl Db, filter: SeriesFilter, reason: &str, now: i64) -> Result<u32, CoreError>;
pub fn queue_length(db: &impl Db) -> Result<u32, CoreError>;
pub async fn process_queue<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome;   // StepOutcome from Task 1
```

Rules:
- `resolve_leads` (§7.6): source `notify = false` or source disabled → `Muted`; `lead_minutes == Some([])` → `Muted`; non-empty explicit → those; else `honor_valarm` && non-empty `valarm_minutes` → those; else source `default_lead_min`; else global. Deduplicate and sort.
- `plan_reminders`: `Muted` → empty. Timed: one per lead, `fire_at = start − lead·60_000`, key `<occ_key>|<lead>`. All-day: one at `all_day_time` on the occurrence's first date in its zone, key `<occ_key>|allday`, `lead_min = None`.
- `reconcile_series`: load series → if empty, delete its pending reminders (withdraw rule) and return `Gone`; expand `[now, now + 60 d)` with `anchor_before_utc = now − ANCHOR_LAG_MS`; persist anchor/status/warning on the parent; plan; diff exactly per the §7.3 table (including the `withdrawn: event changed or removed` delivery when `attempts > 0`, `channel = 'bark'`, `kind = 'reminder'`, `ok = 0`); on `CatchingUp`, re-enqueue with reason `catch_up` (attempts 0, `not_before = now`).
- `process_queue` (§7.1 step 2): select ≤ `QUEUE_BATCH` items with `not_before <= now` ordered by `not_before`; for each item: **trace first** (`attempts += 1`, `not_before = now + 2^attempts min`, `last_error = 'interrupted'`), `commit_point().await`, then `db.transaction(|tx| reconcile_series(tx, key, now))`; success → delete the item (unless reconcile re-enqueued it for `catch_up`, which resets it); `Err`/panic (caught per item) → `last_error` = message; if `attempts >= 3` → delete the item, set the parent's `expand_warning = "Reminder recomputation failed: <msg>"`, log. Failure to select or to write a trace → `step_error`.

- [x] **Step 1: Write the failing tests**

```rust
// plan.rs unit tests
#[test] fn explicit_leads_win() {}
#[test] fn valarm_used_only_when_honored() {}
#[test] fn source_default_then_global() {}
#[test] fn empty_explicit_list_mutes() { assert!(matches!(resolve_leads(&row_with(Some(vec![])), None, 15), Leads::Muted)); }
#[test] fn source_notify_off_mutes() {}
#[test] fn all_day_single_reminder_at_nine_local() { /* Berlin all-day 2026-12-25 → fire_at == ms("2026-12-25T08:00:00Z"), key ends "|allday" */ }
// kairos-testkit/tests/reconcile.rs
#[tokio::test] async fn upsert_then_reconcile_inserts_pending() {}
#[tokio::test] async fn changed_time_rewrites_sent_row_as_pending() {}
#[tokio::test] async fn removed_with_attempts_logs_withdrawn_delivery() {}
#[tokio::test] async fn unchanged_upsert_is_noop() {}
#[tokio::test] async fn queue_drains_ten_per_call() { /* enqueue 25 → 3 calls → queue_length 0 */ }
#[tokio::test] async fn queue_item_dropped_after_three_failures_with_warning() { /* FaultInjector panics in reconcile for one series → after 3 calls (clock advanced past not_before) item gone, expand_warning starts "Reminder recomputation failed:" */ }
#[tokio::test] async fn catching_up_requeues_until_complete() { /* hourly since 2020 → repeated process_queue → status Complete, pending reminders exist, never dropped */ }
#[tokio::test] async fn muted_source_withdraws_pending() {}
```

- [ ] **Step 2: Run** `cargo test -p kairos-core calendar::plan && cargo test -p kairos-testkit --test reconcile` — FAIL.
- [x] **Step 3: Implement** the modules.
- [x] **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: event store, reminder planning, reconciliation and reconcile queue`

---

### Task 7: Messages and the Bark channel

**Files:**
- Create: `crates/kairos-core/src/notify/{message.rs, bark.rs, mod.rs}`
- Test: unit tests in `message.rs`; `crates/kairos-testkit/tests/bark.rs`

**Interfaces:**
- Produces:
```rust
pub struct Message { pub title: String, pub body: String, pub url: Option<String> }
pub struct ReminderView<'a> { pub title: &'a str, pub location: Option<&'a str>, pub source_name: Option<&'a str>,
    pub all_day: bool, pub occ_start_utc: i64, pub zone: &'a str, pub now: i64, pub late: bool, pub event_id: i64 }
pub fn format_reminder(v: &ReminderView, origin: &str) -> Message;
pub fn format_source_alert(source_name: &str, error: &str, origin: &str) -> Message;
pub struct BarkConfig { pub server: String, pub device_key: String }        // server default "https://api.day.app"
pub fn load_channel(db: &impl Db) -> Result<Option<BarkConfig>, CoreError>; // channels row kind='bark', enabled=1
pub enum ChannelError { Http(u16), Api(i64, String), Net(String) }
pub async fn send_bark(http: &impl Http, cfg: &BarkConfig, msg: &Message) -> Result<(), ChannelError>;
```
Exact copy (§10): title `⏰ <title>`; body `HH:mm · in N min` / `HH:mm · now` (when `now >= occ_start_utc − 60_000`) / `Today · all day`, then ` · <location>` and ` · <source>` when present; `(late) ` prefix on the title when `late`; url `<origin>/calendar?event=<id>`. Alert: title `⚠️ Source sync failing`, body `<name>: <error truncated to 200 chars>`, url `<origin>/sources`. Bark request: `POST <server>/push`, JSON `{device_key,title,body,url,group:"reminders",level:"timeSensitive"}`, `max_body_bytes = 65_536`; success iff HTTP 200 and JSON `code == 200`.

- [x] **Step 1: Write the failing tests** — `formats_timed_in_n_min`, `formats_now_for_lead_zero`, `formats_all_day`, `late_prefix`, `alert_truncates_error_to_200_chars` (exact strings); `bark_posts_expected_json` (inspect `FakeHttp::requests()`), `bark_http_500_is_error`, `bark_code_400_in_200_response_is_error`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: reminder messages and Bark channel`

---

### Task 8: Source sync and source alerts

**Files:**
- Create: `crates/kairos-core/src/sources/sync.rs`
- Test: `crates/kairos-testkit/tests/sync.rs`, fixtures

**Interfaces:**
- Consumes: `parse_ics` (Task 4); store + queue (Task 6); `send_bark`, `format_source_alert`, `load_channel` (Task 7).
- Produces:
```rust
pub const MAX_ICS_BYTES: usize = 5 * 1024 * 1024;
pub fn pick_due_source(db: &impl Db, now: i64) -> Result<Option<SourceRow>, CoreError>;   // enabled; never synced or last_sync_at + interval <= now; oldest due first
pub async fn sync_due_source<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome;
pub async fn sync_source<P: Ports>(hub: &Hub<P>, source: &SourceRow, now: i64) -> Result<SyncSummary, String>;   // Err = recorded failure text
pub async fn send_source_alerts<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome;          // §7.1 step 3
pub struct SyncSummary { pub events: u32, pub enqueued: u32, pub unchanged: bool }
```
Rules (§7.5): trace first (`last_sync_at = now`, `consecutive_failures += 1`, `last_error = 'interrupted before completion (CPU or memory limit?)'`); conditional GET with stored validators, `max_body_bytes = MAX_ICS_BYTES`; reject `Content-Length > MAX_ICS_BYTES` and `NetError::TooLarge` with `feed too large (> 5 MB)`; non-2xx → `HTTP <status>`; `NotCalendar` → `not a calendar (no VCALENDAR)`; `InvalidUtf8` → `feed is not valid UTF-8`; `304` → success `unchanged`. Success: one transaction — upsert rows, prune rows of this source not in the feed (whole series and stale overrides), enqueue changed/created/pruned series (`sync`), store validators, `consecutive_failures = 0`, `alerted = 0`, `last_error = NULL`, `last_result = "ok: N events"` + `"; " + warnings.join("; ")` when any. Failure: overwrite `last_error` only. URL must be `https://` unless `config.allow_http_urls`. `send_source_alerts`: for each source with `consecutive_failures >= 3 AND alerted = 0`: set `alerted = 1` first, then send (no channel → log only), append `deliveries` (`kind='source_alert'`).

- [x] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn first_sync_imports_and_enqueues() {}
#[tokio::test] async fn not_modified_is_unchanged() { /* second sync sends If-None-Match; 304 → last_result "unchanged", failures 0 */ }
#[tokio::test] async fn html_200_is_failure_and_keeps_data() {}
#[tokio::test] async fn oversize_without_content_length_is_failure() {   // Review Focus 5 (core half)
    h.http().respond("https://feeds.test/a.ics", 200, &[], &vec![b'x'; MAX_ICS_BYTES + 1]);
    sync_due_source(&h, h.now()).await;
    assert_eq!(src(&h).last_error.as_deref(), Some("feed too large (> 5 MB)"));
}
#[tokio::test] async fn prunes_removed_series_and_stale_overrides() {}
#[tokio::test] async fn cancelled_instance_never_gets_a_reminder() {}
#[tokio::test] async fn panicking_parse_still_advances_last_sync_at() { /* FaultInjector panic in upsert → last_sync_at == now, failures 1, last_error not "interrupted" */ }
#[tokio::test] async fn killed_sync_leaves_interrupted_trace_and_is_not_redone() { /* FaultInjector kill after trace → source not due on replay; last_error starts "interrupted" */ }
#[tokio::test] async fn third_failure_alerts_once_even_if_killed() { /* 3 failures (last one killed) → next send_source_alerts sends 1 Bark push; a 4th failure sends none */ }
#[tokio::test] async fn http_url_rejected_in_production_config() {}
#[tokio::test] async fn rdate_and_unknown_tzid_warnings_in_last_result() {}
```

- [ ] **Step 2: Run** `cargo test -p kairos-testkit --test sync` — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: source sync with pessimistic traces and source alerts`

---

### Task 9: Dispatch, alarm cycle, re-arm and backoff, maintenance, watchdog

**Files:**
- Create: `crates/kairos-core/src/sched/dispatch.rs`; Modify: `crates/kairos-core/src/sched/alarm.rs` (replace stubs), `crates/kairos-core/src/http/internal.rs` (`/internal/rearm`), `crates/kairos-worker/src/lib.rs` (`#[event(scheduled)]`)
- Test: `crates/kairos-testkit/tests/{dispatch.rs, alarm.rs}`

**Interfaces:**
- Consumes: Tasks 6–8.
- Produces:
```rust
// dispatch.rs
pub const DISPATCH_BATCH: usize = 20;
pub const MAX_ATTEMPTS: u32 = 4;
pub const LATE_THRESHOLD_MS: i64 = 120_000;
pub const STARTED_GRACE_MS: i64 = 120_000;
pub fn retry_delay_ms(attempt_after_increment: u32) -> i64;   // 1 → 60_000, 2 → 300_000, 3 → 900_000, ≥4 → 900_000 (safety only)
pub fn already_started(occ_start_utc: i64, all_day: bool, duration_sec: i64, now: i64) -> bool;  // §7.6
pub async fn dispatch_due<P: Ports>(hub: &Hub<P>, now: i64) -> StepOutcome;
// alarm.rs
pub const ERROR_FLOOR_MAX_MIN: i64 = 15;
pub async fn run_step<F: Future<Output = StepOutcome>>(name: &str, f: F) -> StepOutcome;   // catch_unwind; a panic → step_error "panic: <msg>"
pub fn compute_next_alarm(db: &impl Db, now: i64) -> Result<i64, CoreError>;
pub fn run_maintenance(db: &impl Db, now: i64) -> Result<(), CoreError>;
pub enum WatchdogAction { AlarmPresent, Rearmed }
pub async fn watchdog<P: Ports>(hub: &Hub<P>) -> WatchdogAction;
// Hub methods (replace Task 1 stubs)
pub async fn run_alarm(&self) -> AlarmReport;
pub async fn rearm(&self, floor: Floor);
```
- `already_started`: timed → `now > occ_start_utc + STARTED_GRACE_MS`; all-day → `now >= occ_start_utc + duration_sec * 1000` (the event's last day is over).
- `dispatch_due`: exactly the 7 steps of §7.6 "Dispatch of one reminder" (trace first using pre-trace `a`; then missing event / `a >= 4` / started / no channel; send; outcome; one `deliveries` row per attempt with `kind='reminder'`, `channel='bark'`). Each item is wrapped so an `Err` or panic after the trace counts as a failed attempt (`last_error`), and the step continues. Late = `now − fire_at > LATE_THRESHOLD_MS`.
- `run_alarm`: `now = self.now()`; steps in order through `run_step`: `dispatch`, `queue`, `source_alerts`, `sync`, `maintenance` (only if `meta:last_maintenance_at` absent or `+ 24 h <= now`; trace `last_maintenance_at = now − 23 h` + `commit_point()` first; on error set `meta:last_maintenance_error`); the whole sequence inside a top-level catch; then `rearm(Floor::AlarmError)` if any step reported a `step_error` or the top level failed, else `rearm(Floor::AlarmClean)`; record `meta:last_alarm_error` (message) and the report.
- `rearm(floor)`: streak (`meta:alarm_error_streak`) — `Http` leaves it alone, `AlarmClean` sets 0 (writes only if it was non-zero), `AlarmError` adds 1. Floor time — `Http`/`AlarmClean`: `now + 1000`; `AlarmError`: `now + min(2^(streak − 1), 15) · 60_000`. `target = max(next, floor)` where `next = compute_next_alarm(now)` (it always has a value, because the maintenance term is always present). `alarm.set(target)` only if `alarm.get() != Some(target)`. A `compute_next_alarm` error is treated as `next = now + 15 min` and logged.
- `compute_next_alarm` = min over: pending `next_attempt_at`; queue `not_before`; enabled sources `COALESCE(last_sync_at + interval_min·60_000, now)`; `COALESCE(last_maintenance_at + 86_400_000, now)`.
- `run_maintenance` (§7.1): enqueue recurring series and events newly entering the 60-day window with reason `roll`; delete `deliveries` older than 90 d and non-pending reminders whose `fire_at` is older than 90 d; delete expired sessions; delete `auth_failures` older than 1 h; `last_maintenance_at = now`; clear `last_maintenance_error`.
- `watchdog`: `alarm.get()` is `Some` → `AlarmPresent` (no writes); else `rearm(Floor::Http)`, set `meta:last_watchdog_at = now` and `meta:last_alarm_error = "alarm was missing; re-armed by watchdog"` → `Rearmed`. Route: `POST /internal/rearm` (no auth; reachable only via the entry Worker). Entry `scheduled` handler: stub `fetch` `POST https://hub/internal/rearm`.

- [x] **Step 1: Write the failing tests**

```rust
// dispatch.rs
#[tokio::test] async fn sends_once_even_if_alarm_runs_twice() {}
#[tokio::test] async fn never_sends_for_deleted_event() {}
#[tokio::test] async fn lead_zero_reminder_is_sent_as_now() { /* fire_at == occ_start; alarm runs 3 s late → sent, body contains "· now" */ }
#[tokio::test] async fn timed_skip_after_grace() { /* now = occ_start + 121 s, attempts 0 → skipped */ }
#[tokio::test] async fn multi_day_all_day_sent_late_on_day_two_skipped_after_end() {   // Review Focus 3
    // 3-day all-day event from 2026-11-21 (UTC user zone); reminder planned at 09:00 on 11-21 only
    // clock 2026-11-22T10:00Z → sent with "(late) "; second copy of the scenario at 2026-11-24T00:00Z → skipped
}
#[tokio::test] async fn retries_at_1_5_15_then_fails_after_4_attempts() {
    // failing Bark; record next_attempt_at after each attempt
    assert_eq!(next_attempts, [ms(NOW) + 60_000, ms(NOW) + 360_000, ms(NOW) + 1_260_000]);
    assert_eq!(h.http().requests().len(), 4); assert_eq!(status(&h), "failed");
}
#[tokio::test] async fn killed_send_is_retried_not_looped() { /* kill after trace → replay at same now sends nothing; at now+1 min sends once */ }
#[tokio::test] async fn interrupted_fourth_attempt_becomes_failed() {}
#[tokio::test] async fn no_channel_fails() {}
// alarm.rs
#[tokio::test] async fn arms_at_earliest_work() { /* reminder +10 min, source due +30 min → alarm_at == now + 600_000 */ }
#[tokio::test] async fn arms_within_a_second_while_queue_nonempty() {}
#[tokio::test] async fn no_redundant_set_alarm() { /* two rearm calls → set_alarm_calls == 1 */ }
#[tokio::test] async fn every_step_failing_or_panicking_backs_off() {
    for fault in ["fail", "panic"] { for step in ["dispatch","queue","source_alerts","sync","maintenance"] {
        // inject the fault into the step's selection query; run_alarm
        assert!(h.alarm_at().unwrap() >= h.now() + 60_000, "{step}/{fault}");
    }}
}
#[tokio::test] async fn item_level_sync_failure_does_not_raise_floor() { /* broken feed only → alarm at the next reminder, streak 0 */ }
#[tokio::test] async fn error_floor_grows_1_2_4_8_15_and_resets() {}
#[tokio::test] async fn http_rearm_does_not_reset_streak() {}
#[tokio::test] async fn failing_maintenance_retries_hourly() {}
#[tokio::test] async fn maintenance_rolls_recurring_series_and_purges_old_rows() {}
#[tokio::test] async fn watchdog_noop_when_alarm_present_and_rearms_when_missing() { /* present → 0 writes (compare total_changes); missing → alarm set, meta written */ }
```

- [ ] **Step 2: Run** `cargo test -p kairos-testkit --test dispatch --test alarm` — FAIL.
- [x] **Step 3: Implement**; wire `Hub::run_alarm`/`rearm`; add the entry `scheduled` handler.
- [x] **Step 4: Run** — PASS; `cargo xtask e2e` still PASS.
- [x] **Step 5: Commit** — `feat: dispatch, isolated alarm steps, backoff and watchdog`

---

### Task 10: Auth primitives

**Files:**
- Create: `crates/kairos-core/src/auth/{crypto.rs, cookies.rs, session.rs, csrf.rs, recovery.rs, challenge.rs, mod.rs}`
- Test: unit tests in `crypto.rs`, `cookies.rs`, `challenge.rs`; `crates/kairos-testkit/tests/auth_primitives.rs`

**Interfaces:**
- Produces:
```rust
pub fn random_bytes<const N: usize>(e: &impl Entropy) -> [u8; N];
pub fn b64url(bytes: &[u8]) -> String;  pub fn b64url_decode(s: &str) -> Result<Vec<u8>, CoreError>;
pub fn sha256(bytes: &[u8]) -> [u8; 32];  pub fn sha256_hex(bytes: &[u8]) -> String;
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool;                                   // subtle
pub fn sign(secret: &[u8], payload: &[u8]) -> String;                       // "<b64url payload>.<b64url hmac-sha256>"
pub fn verify(secret: &[u8], token: &str) -> Option<Vec<u8>>;
pub fn parse_cookies(header: Option<&str>) -> Vec<(String, String)>;
pub fn set_cookie(name: &str, value: &str, max_age_s: Option<i64>) -> (String, String);   // "Set-Cookie", "...; HttpOnly; Secure; SameSite=Strict; Path=/"
pub enum Scope { Full, Recovery }
pub struct Session { pub id_hash: String, pub scope: Scope, pub expires_at: i64 }
pub fn create_session<P: Ports>(hub: &Hub<P>, scope: Scope) -> Result<(Session, String /*cookie value*/), CoreError>;
pub fn read_session<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Option<Session>, CoreError>;   // slides expiry at most once per day
pub fn upgrade_session(db: &impl Db, s: &Session) -> Result<(), CoreError>;  pub fn revoke_session(...);  pub fn revoke_all_sessions(...);
pub fn check_origin(req: &Request, origin: &str) -> Result<(), CoreError>;   // state-changing methods only → Forbidden
pub fn check_recovery_code<P: Ports>(hub: &Hub<P>, code: &str) -> Result<(), CoreError>;  // TooManyRequests / Unauthorized
pub enum Purpose { Reg, Auth }
pub fn issue_challenge<P: Ports>(hub: &Hub<P>, purpose: Purpose) -> ([u8; 32], (String, String) /*Set-Cookie*/);
pub fn take_challenge<P: Ports>(hub: &Hub<P>, req: &Request, purpose: Purpose) -> Result<([u8; 32], (String, String) /*clearing Set-Cookie*/), CoreError>;
```
Values: cookie names `__Host-sid`, `__Host-chal`; session 32 random bytes, stored as SHA-256 hex; TTL 30 d, extended at most once per 24 h; challenge payload `{purpose, challenge, exp}` with `exp = now + 120_000`; recovery: 5 failures within 1 h → `meta:recovery_locked_until = now + 3_600_000`, failures recorded in `auth_failures`.

- [x] **Step 1: Write the failing tests** — `sign_verify_round_trip_and_tamper`, `ct_eq_lengths`, `session_slides_once_per_day`, `expired_session_is_none`, `origin_required_for_post_not_get`, `recovery_locks_after_five_failures_for_one_hour` (6th attempt with the right code → 429; at +61 min the right code passes), `challenge_wrong_purpose_rejected`, `challenge_expires_after_two_minutes`, `challenge_is_single_use` (clearing cookie returned).
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: sessions, CSRF, recovery code and signed challenges`

---

### Task 11: WebAuthn verifier and software authenticator

**Files:**
- Create: `crates/kairos-core/src/webauthn/{mod.rs, authdata.rs, cose.rs, verify.rs, options.rs}`, `crates/kairos-testkit/src/authenticator.rs`
- Test: `crates/kairos-testkit/tests/webauthn.rs`

**Interfaces:**
- Produces:
```rust
// wire types (kairos-types/src/auth.rs, base64url strings as in the WebAuthn JSON encoding).
// JSON names are exact and need explicit #[serde(rename)]: id, rawId, type, response, clientDataJSON,
// attestationObject, transports, authenticatorData, signature, userHandle (camelCase would produce "clientDataJson").
pub struct RegistrationResponseJson { pub id: String, pub raw_id: String, pub r#type: String, pub response: AttestationResponseJson }
pub struct AttestationResponseJson { pub client_data_json: String, pub attestation_object: String, pub transports: Option<Vec<String>> }
pub struct AuthenticationResponseJson { pub id: String, pub raw_id: String, pub r#type: String, pub response: AssertionResponseJson }
pub struct AssertionResponseJson { pub client_data_json: String, pub authenticator_data: String, pub signature: String, pub user_handle: Option<String> }
pub struct CreationOptionsJson { /* rp {id,name}, user {id,name,displayName}, challenge, pubKeyCredParams [-7,-8,-257], authenticatorSelection {residentKey:"required", userVerification:"preferred"}, attestation:"none", excludeCredentials, timeout 120000 */ }
pub struct RequestOptionsJson { /* challenge, rpId, userVerification:"preferred", allowCredentials: [] , timeout 120000 */ }
// kairos-core/webauthn
pub struct RpContext<'a> { pub rp_id: &'a str, pub origin: &'a str }
pub struct NewCredential { pub id: String, pub public_key: Vec<u8>, pub alg: i64, pub counter: u32, pub transports: Vec<String> }
pub struct StoredCredential { pub id: String, pub public_key: Vec<u8>, pub alg: i64, pub counter: u32 }
pub enum WebauthnError { /* one variant per numbered check in §11.2, e.g. BadType, IdMismatch, IdTooLong, ClientDataType, Challenge, Origin, CrossOrigin, Cbor, RpIdHash, UserPresence, NoAttestedData, TrailingBytes, CredIdMismatch, UnsupportedAlgorithm, WeakRsaKey, BadKey, UserHandle, Signature, CounterRegression, UnknownCredential */ }
pub fn creation_options(rp: &RpContext, user_id: &[u8; 16], challenge: &[u8; 32], exclude: &[String]) -> CreationOptionsJson;
pub fn request_options(rp: &RpContext, challenge: &[u8; 32]) -> RequestOptionsJson;
pub fn verify_registration(rp: &RpContext, challenge: &[u8; 32], resp: &RegistrationResponseJson) -> Result<NewCredential, WebauthnError>;
pub fn verify_authentication(rp: &RpContext, challenge: &[u8; 32], user_id: &[u8; 16], cred: &StoredCredential, resp: &AuthenticationResponseJson) -> Result<u32 /*new counter*/, WebauthnError>;
// kairos-testkit/authenticator.rs
pub enum Alg { Es256, EdDsa, Rs256 }
pub struct SoftAuthenticator;  impl SoftAuthenticator {
    pub fn new(alg: Alg, seed: u64) -> Self;
    pub fn register(&mut self, opts: &CreationOptionsJson, origin: &str) -> RegistrationResponseJson;
    pub fn login(&mut self, opts: &RequestOptionsJson, origin: &str) -> AuthenticationResponseJson;   // increments its counter
    pub fn tamper(&mut self) -> &mut Tamper;   // knobs used by negative tests (flags, rp id hash, origin, type, crossOrigin, trailing bytes, extensions, counter, signature)
}
```
Checks: exactly the numbered lists in §11.2 (registration 1–8, authentication 1–6), in that order; signature before counter; ES256 DER signatures normalised to low-S before `p256` verification; EdDSA via `verify_strict`; RS256 PKCS#1 v1.5 SHA-256 with modulus ≥ 2,048 bits; `fmt`/`attStmt` ignored; extensions map skipped when `ED` is set.

- [x] **Step 1: Write the failing tests** — for each `Alg`: `registers_and_logs_in_<alg>`; negative tests, one per variant: `rejects_wrong_origin`, `rejects_wrong_challenge`, `rejects_create_type_on_login`, `rejects_cross_origin_true`, `rejects_wrong_rp_id_hash`, `rejects_user_presence_clear`, `rejects_trailing_bytes`, `accepts_extensions_map`, `accepts_uv_clear` (UV not required), `rejects_unsupported_alg` (P-384), `rejects_rsa_1024`, `rejects_id_mismatch`, `rejects_tampered_signature`, `rejects_counter_regression`, `accepts_zero_counters`, `accepts_high_s_es256`, `rejects_wrong_user_handle`.
- [ ] **Step 2: Run** `cargo test -p kairos-testkit --test webauthn` — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: in-house WebAuthn relying-party verification`

---

### Task 12: Auth routes

**Files:**
- Create: `crates/kairos-core/src/http/auth_routes.rs`; Modify: `http/router.rs`, `crates/kairos-types/src/auth.rs`, `crates/kairos-testkit/src/lib.rs` (`admin_session`)
- Test: `crates/kairos-testkit/tests/auth_routes.rs`

**Interfaces:**
- Consumes: Tasks 10–11.
- Produces routes (§12) and DTOs: `GET /auth/state` → `AuthState { setup_required: bool, session: Option<"full"|"recovery">, passkeys: u32 }` (`setup_required` = no passkey with `rp_id == RP_ID`); `POST /auth/recovery { code }` → recovery session; `POST /auth/passkey/register/options` (requires a full **or** recovery session — recovery is allowed at any time, which covers both first-time setup and a lost device, §11.3) → `CreationOptionsJson` + challenge cookie; `POST /auth/passkey/register/verify { response, name? }` → stores key (name default `Passkey <n>`), recovery session upgraded to full → `{ id }`; `POST /auth/passkey/login/options` → `RequestOptionsJson`; `POST /auth/passkey/login/verify { response }` → full session; `POST /auth/logout`, `POST /auth/logout-all`. `meta:webauthn_user_id` created on first registration (16 random bytes). Origin check on every POST. These routes change nothing the alarm depends on, so they do not call `rearm`.
- Testkit: `TestHub::admin_session()` = recovery → register with `SoftAuthenticator(Es256)` → returns the full-session cookie.

- [x] **Step 1: Write the failing tests** — `fresh_deployment_requires_recovery_for_first_key`, `registration_without_session_is_401`, `recovery_session_can_only_register` (`GET /api/admin/settings` → 403), `register_then_login_with_passkey`, `stale_rp_id_key_not_counted_and_cannot_log_in` (insert a key with `rp_id='old.test'` → `setup_required: true`, login with it → 400 `unknown_credential`), `post_without_origin_is_403`, `logout_all_revokes_every_session`, `challenge_replay_rejected`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: passkey and recovery auth routes`

---

### Task 13: API tokens and public API v1

**Files:**
- Create: `crates/kairos-core/src/auth/tokens.rs`, `crates/kairos-core/src/http/{api_v1.rs, validate.rs}`, `crates/kairos-types/src/{api_v1.rs, limits.rs}`
- Test: `crates/kairos-testkit/tests/api_v1.rs`

**Interfaces:**
- Produces:
```rust
// kairos-types/api_v1.rs
#[serde(deny_unknown_fields)] pub struct EventPayload { pub title: String, pub start: String, pub end: Option<String>, pub all_day: Option<bool>,
    pub timezone: Option<String>, pub location: Option<String>, pub description: Option<String>,
    pub rrule: Option<String>, pub exdates: Option<Vec<String>>, pub reminders: Option<Vec<u32>> }
pub struct PutEventResponse { pub id: i64, pub created: bool, pub reminders_scheduled: u32 }
pub struct OccurrenceDto { pub occ_key: String, pub origin: String, pub external_id: Option<String>, pub source: Option<String>,
    pub title: String, pub location: Option<String>, pub all_day: bool, pub start_utc: i64, pub end_utc: i64,
    pub start_local: String, pub end_local: String }
pub struct ListEventsResponse { pub occurrences: Vec<OccurrenceDto>, pub truncated: Vec<String> }
// kairos-core
pub fn create_token<P: Ports>(hub: &Hub<P>, name: &str) -> Result<(i64, String /*kr_… shown once*/), CoreError>;
pub fn verify_bearer<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<i64 /*token id*/, CoreError>;   // updates last_used_at at most once per hour
pub fn list_tokens(db: &impl Db) -> Result<Vec<TokenInfo>, CoreError>;  pub fn revoke_token(db: &impl Db, id: i64) -> Result<(), CoreError>;
pub fn validate_payload(p: &EventPayload, external_id: &str, user_tz: &str) -> Result<EventRow, CoreError>;   // CoreError::Invalid { field, … }
pub fn list_occurrences<P: Ports>(hub: &Hub<P>, from: i64, to: i64, include_disabled: bool) -> Result<(Vec<OccurrenceDto>, Vec<String>), CoreError>;  // shared with Task 14
```
Rules (§12): PUT = full replacement → `upsert_event` + inline `reconcile_series` in one transaction → `rearm`; `start` parsed with `parse_api_instant(start, timezone.unwrap_or(user_tz))`; `exdates` only with `rrule`; `reminders: []` → `lead_minutes = Some(vec![])`; absent → `None`; `rrule` validated with `validate_rrule`; DELETE → `204` / `404`, inline withdraw; GET range ≤ 92 days, hides disabled sources, `start_local`/`end_local` in the user zone. Tokens `kr_` + b64url(32 bytes); stored SHA-256 hex + 10-char prefix. API tokens may only write `api` events (other series keys are unreachable by construction: `series_key = "api:<external_id>"`).

- [x] **Step 1: Write the failing tests**

```rust
#[tokio::test] async fn put_creates_then_unchanged_put_is_noop() { /* 200 created true → same body → created false, reminders_scheduled same */ }
#[tokio::test] async fn put_is_full_replacement() { /* first with location, second without → location None */ }
#[tokio::test] async fn explicit_offset_stored_in_user_zone_and_instant_survives_zone_change() {   // Review Focus 2
    // user zone Europe/Berlin; PUT start "2026-11-20T09:00:00+08:00" reminders [0]
    // events row: start_local "2026-11-20T02:00:00", tzid "Europe/Berlin"; fire_at == ms("2026-11-20T01:00:00Z")
    // PATCH settings timezone Asia/Tokyo (Task 14 route; here set via put_settings + enqueue + drain) → fire_at unchanged
}
#[tokio::test] async fn empty_reminders_means_none() {}
#[tokio::test] async fn exdates_without_rrule_is_400() { /* { error: "invalid", field: "exdates" } */ }
#[tokio::test] async fn validation_limits() { /* title 201 chars, 6 reminders, 10_081, bad external_id, unknown field → 400 with field names */ }
#[tokio::test] async fn get_returns_expanded_occurrences_sorted_with_truncated() {}
#[tokio::test] async fn get_range_over_92_days_is_400() {}
#[tokio::test] async fn delete_withdraws_and_404s_after() {}
#[tokio::test] async fn revoked_token_is_401() {}
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: API tokens and public events API v1`

---

### Task 14: Admin API

**Files:**
- Create: `crates/kairos-core/src/http/admin/{mod.rs, calendar.rs, sources.rs, events.rs, settings.rs, channel.rs, passkeys.rs, tokens.rs, log.rs, queue.rs, export.rs}`, `crates/kairos-core/src/export.rs`, `crates/kairos-types/src/{admin.rs, repeat.rs}`
- Test: `crates/kairos-testkit/tests/admin.rs`; unit tests in `kairos-types/src/repeat.rs`

**Interfaces:**
- Consumes: everything above; `list_occurrences` (Task 13).
- Produces (all under `full` session + Origin; DTOs in `kairos-types/admin.rs`):
  - `GET /api/admin/calendar?from&to` → `ListEventsResponse` plus per occurrence `event_id`, `color`, `deletable` (manual/api), `editable` (manual), `reminders: [{ key, fire_at, status }]`.
  - `sources`: `GET` list; `POST` create (`https://` only unless `allow_http_urls`; `interval_min ≥ 15`); `PATCH /:id` (name, url, color, enabled, interval_min, default_lead_min, honor_valarm, notify) → `enqueue_where(Source(id))`, response includes `queued`; `DELETE /:id` → delete its events (withdraw via queue) and the source; `POST /:id/sync` → sync it now inline (one source) and return the row.
  - `events`: `POST` manual create (`ManualEventInput { title, start, end?, all_day, timezone?, location?, description?, repeat: Option<RepeatPreset>, reminders: ReminderChoice }`) → inline reconcile; `PATCH /:id` manual only (whole series); `DELETE /:id` manual or api; `POST /:id/skip { slot }` manual recurring only → `add_exdate` + inline reconcile; api → `409 { error: "not_manual" }`.
  - `settings`: `GET`; `PUT { timezone, default_lead_min, all_day_time }` → validates zone/`HH:mm` → enqueue `FloatingOrAllDay` on zone/all-day-time change, `All` on default-lead change → `{ queued }`.
  - `channel`: `GET` (device key masked to the last 4 chars), `PUT { server, device_key, enabled }`, `POST test` → sends a test message and logs `kind='test'`.
  - `passkeys`: `GET` (`stale` flag when `rp_id != RP_ID`), `PATCH /:id { name }`, `DELETE /:id[?confirm=1]` → `409 { error: "confirm_required", warning: "This is your last passkey for <RP_ID>. Without it you can only sign in with the recovery code." }` when it is the last valid key and `confirm` is absent.
  - `tokens`: `GET`, `POST { name }` → `{ id, token }` (once), `DELETE /:id`.
  - `log`: `GET` → `{ failed_reminders, deliveries (newest 200), failing_sources, expansion_warnings, background: { alarm_error_streak, last_alarm_error, last_maintenance_at, last_maintenance_error, last_watchdog_at } }`.
  - `queue`: `GET` → `{ length }`.
  - `export.json` (no passkeys, sessions, tokens; Bark key masked) and `export.ics` (all non-disabled series as VEVENTs with RRULE/EXDATE).
  - `kairos-types/repeat.rs`: `RepeatPreset { freq: Daily|Weekly{days}|Monthly|Yearly, interval: u8 (1–99), end: Never|Until(date)|Count(u16 1–999) }`; `to_rrule(&RepeatPreset) -> String` (Monthly uses the start's day of month: `FREQ=MONTHLY;BYMONTHDAY=<d>`); `from_rrule(&str, start_date) -> Option<RepeatPreset>` (inverse for editing).
  - `ReminderChoice { Default, Custom(Vec<u32>), None }` → `lead_minutes` `None` / `Some(v)` / `Some(vec![])`.

- [x] **Step 1: Write the failing tests** — `admin_requires_full_session`, `calendar_marks_deletable_and_editable`, `calendar_start_local_follows_user_zone_across_dst` (**Review Focus 4**: user `Asia/Shanghai`, weekly NY 09:00 from 2026-10-19, window 2026-10-19..2026-11-15 → `start_local` times `21:00` for 10-19/10-26 and `22:00` for 11-02/11-09 — NY falls back on 11-01, so Shanghai sees the later hour after it), `disabled_source_hidden_and_reminders_withdrawn_after_drain`, `notify_off_withdraws_reminders`, `settings_zone_change_queues_floating_only`, `manual_weekly_preset_creates_rrule`, `skip_one_occurrence_of_manual_series`, `skip_on_api_series_is_409`, `last_passkey_delete_needs_confirm`, `passkey_rename`, `channel_test_logs_test_delivery`, `token_shown_once`, `log_contains_background_section`, `queue_length_reports_pending_items`, `export_json_excludes_secrets`, `export_ics_round_trips_through_parse_ics`; `repeat.rs`: `to_rrule_from_rrule_round_trip` for each preset.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [x] **Step 5: Commit** — `feat: admin API for the web app`

---

### Task 15: Web app shell, API client, passkeys in the browser, auth screens

**Files:**
- Create: `crates/kairos-web/{Cargo.toml, Trunk.toml, index.html, boot.js, _headers, style.css}`, `crates/kairos-web/src/{main.rs, app.rs, api.rs, passkey.rs, b64.rs, strings.rs, pages/setup.rs, pages/login.rs}`
- Modify: `xtask/src/main.rs` (`build`: `trunk build --release` in `crates/kairos-web`, then worker build; `test`: add `cargo test -p kairos-web` and the `inner_html` check: fail if `grep -R "inner_html" crates/kairos-web/src` finds anything)
- Test: unit tests in `b64.rs`, `api.rs` (URL building and error mapping only)

**Interfaces:**
- Consumes: `kairos-types` DTOs (Tasks 12–14).
- Produces:
```rust
pub enum ApiError { Unauthorized, Forbidden, NotFound, Conflict { code: String, warning: String }, Invalid { field: String, message: String }, TooManyRequests, Network(String), Server(u16) }
pub async fn get<T: DeserializeOwned>(path: &str) -> Result<T, ApiError>;
pub async fn send<B: Serialize, T: DeserializeOwned>(method: &str, path: &str, body: &B) -> Result<T, ApiError>;   // same-origin, credentials included
pub async fn create_passkey(opts: CreationOptionsJson) -> Result<RegistrationResponseJson, String>;  // navigator.credentials.create via web-sys
pub async fn get_passkey(opts: RequestOptionsJson) -> Result<AuthenticationResponseJson, String>;
pub fn browser_time_zone() -> String;                 // Intl.DateTimeFormat().resolvedOptions().timeZone via js-sys
```
- `Trunk.toml`: `[build] target = "index.html"`, `dist = "dist"`, `filehash = false`, `inject_scripts = false` (verify the key names against the resolved Trunk version; record in the notes). `index.html` has `<link data-trunk rel="rust" data-bin="kairos-web" />`, `copy-file` links for `boot.js` and `_headers`, `<link data-trunk rel="css" href="style.css" />`, and `<script type="module" src="/boot.js"></script>`.
- `boot.js` (exact):
```js
import init from './kairos-web.js';
await init({ module_or_path: './kairos-web_bg.wasm' });
```
(If Trunk emits different file names, use those names; `filehash = false` keeps them stable.)
- `_headers` (exact):
```
/*
  Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'
  X-Content-Type-Options: nosniff
  Referrer-Policy: same-origin
/index.html
  Cache-Control: no-cache
/boot.js
  Cache-Control: no-cache
/kairos-web.js
  Cache-Control: no-cache
/kairos-web_bg.wasm
  Cache-Control: no-cache
```
- Routes and guards (§13.1): `/setup` (step 1 recovery code → step 2 register passkey → save `browser_time_zone()` via `PUT /api/admin/settings` → `/calendar`), `/login` (one button: passkey login), all other pages require a full session; any `ApiError::Unauthorized` navigates to `/login`. Shell: top navigation Calendar · Sources · Events · Settings · Log · Log out. All copy from `strings.rs`.

- [x] **Step 1: Write the failing tests** — `b64url_round_trip_and_no_padding`, `api_maps_409_confirm_required` (pure mapping from status + JSON body), `api_maps_401_to_unauthorized`.
- [ ] **Step 2: Run** `cargo test -p kairos-web` — FAIL. **Step 3: Implement** the shell, `api.rs`, `passkey.rs`, setup and login pages. **Step 4: Run** `cargo xtask build` — PASS, `crates/kairos-web/dist` contains `index.html`, `boot.js`, `_headers`, `kairos-web.js`, `kairos-web_bg.wasm`; open `npx wrangler dev` → setup page loads with no CSP errors in the console (check manually; Task 17 automates it).
- [x] **Step 5: Commit** — `feat: Leptos shell, browser passkeys and setup/login`

---

### Task 16: Web pages

**Files:**
- Create: `crates/kairos-web/src/calendar_grid.rs`, `crates/kairos-web/src/pages/{calendar.rs, sources.rs, events.rs, settings.rs, log.rs}`, `crates/kairos-web/src/components/{event_drawer.rs, confirm_dialog.rs, repeat_editor.rs, reminder_editor.rs}`
- Test: unit tests in `calendar_grid.rs`

**Interfaces:**
- Consumes: Task 14 DTOs, `to_rrule`/`from_rrule`, `api.rs` (Task 15).
- Produces: `pub fn month_grid(year: i16, month: i8) -> Vec<[jiff::civil::Date; 7]>` (Monday-first, 5 or 6 weeks); `pub fn week_of(date) -> [Date; 7]`; `pub fn place(occs: &[OccurrenceDto]) -> BTreeMap<Date, Vec<&OccurrenceDto>>` (by `start_local` date; multi-day all-day events appear on every day they cover).

Pages (§13.1 table — implement every capability listed there):
- **Calendar:** month and week views; source color; `?event=<id>` highlight and open drawer; drawer shows reminder statuses; edit/delete for manual, delete for api, read-only ics; truncated-series hint; recurring manual events offer "Delete this occurrence" (`POST /events/:id/skip`).
- **Sources:** list with red failing rows and `last_result`; add/edit form with interval (≥ 15), default lead, honor VALARM, **Send reminders** switch, color; enable/disable; delete; Sync now.
- **Events:** manual list + form with `repeat_editor` (none/daily/weekly days/monthly/yearly, every N, never/until/count) and `reminder_editor` (Default / custom list / No reminders).
- **Settings:** zone (select from a short list + free text), default lead, all-day time; while `GET /api/admin/queue` > 0 poll every 2 s and show `Recomputing reminders… N left`; Bark config + "Send test notification"; passkeys (add, rename, revoke with `confirm_dialog` on `409 confirm_required`, stale badge); tokens (create shows the token once, revoke); export links.
- **Log:** failed reminders pinned; deliveries; failing sources; expansion warnings; background errors.

- [x] **Step 1: Write the failing tests** — `month_grid_starts_monday` (2026-11 → 6 rows, first cell 2026-10-26, last cell 2026-12-06), `three_day_all_day_placed_on_each_day`, `week_of_sunday_returns_previous_monday` (2026-11-22 → first day 2026-11-16).
- [ ] **Step 2: Run** `cargo test -p kairos-web calendar_grid` — FAIL. **Step 3: Implement** the grid and the pages. **Step 4: Run** — PASS; `cargo xtask build` — PASS.
- [x] **Step 5: Commit** — `feat: calendar, sources, events, settings and log pages`

---

### Task 17: End-to-end, test hooks, deploy tooling, docs and measurements

**Files:**
- Modify: `crates/kairos-core/src/http/internal.rs` (`/__test/clock`, `/__test/alarm`), `crates/kairos-core/src/lib.rs` (`Hub::now` adds `meta:test_clock_offset_ms` only with `test-hooks`), `crates/e2e/src/lib.rs` (browser helper), `xtask/src/main.rs` (`deploy`), `README.md`, `docs/rust-on-workers-notes.md`
- Create: `crates/e2e/tests/smoke.rs`

**Interfaces:**
- Produces: `POST /__test/clock { offset_ms }` → stores the offset; `POST /__test/alarm` → runs `run_alarm()` once and returns the `AlarmReport` as JSON (both exist only with `test-hooks`). `Browser::launch() -> Browser` (chromiumoxide, headless Chrome; `WebAuthn.enable` + `WebAuthn.addVirtualAuthenticator { protocol: "ctap2", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true }`). `xtask deploy`: refuses if `--features test-hooks` would be used or `wrangler.toml` still contains `CHANGE-ME`; runs `build`, `npx wrangler deploy`, prints `startup_time_ms`.

- [x] **Step 1: Write the failing test** `crates/e2e/tests/smoke.rs::full_journey`
  1. `/` → redirected to `/setup`; enter `correct horse`; register a passkey (virtual authenticator); land on `/calendar`; no console messages containing `Content-Security-Policy`.
  2. Settings: Bark server = `MockServer::url("/bark")`, device key `k`; "Send test notification" → mock received one push.
  3. Events: create manual event "Dentist <b>x</b>" today + 2 h, reminders Custom `[30]`.
  4. Calendar shows the title as literal text `Dentist <b>x</b>` (no `<b>` element in the DOM).
  5. `POST /__test/clock { offset_ms: 90 min }` → `POST /__test/alarm` → mock Bark received a push titled `⏰ Dentist <b>x</b>`.
  6. Log page lists the delivery as sent.
  7. Log out → `/login` → passkey login → `/calendar`.
  8. Save the registration and login JSON captured from the network as `crates/kairos-testkit/fixtures/chrome_registration.json` / `chrome_login.json` (first run only; committed) and add `kairos-testkit/tests/webauthn.rs::chrome_fixture_verifies` that verifies them with `rp_id = "localhost"`, `origin = "http://localhost:8799"`, the challenges decoded from each fixture's `clientDataJSON`, and the user id from the registration options.
- [ ] **Step 2: Run** `cargo xtask e2e` — FAIL (hooks and browser helper missing).
- [x] **Step 3: Implement** the hooks, the browser helper, `xtask deploy`, and the README (prerequisites: Rust stable + nightly via `worker-build`, Trunk, Node for wrangler, Chrome for e2e; first deploy: set `RP_ID`/`ORIGIN`, `wrangler secret put RECOVERY_CODE` / `SESSION_SECRET`, `cargo xtask deploy`, open `/setup`; operations: Log page, `wrangler tail --format json`, domain change → recovery flow; Paid plan if CPU limits are hit). If `__Host-` cookies are refused on `http://localhost` by the test browser, record it in the notes and run e2e with `wrangler dev --local-protocol https` instead (and `ORIGIN = "https://localhost:8799"`).
- [ ] **Step 4: Run** `cargo xtask test && cargo xtask e2e` — PASS. Then, on a real deployment (manual, with the user): record in `docs/rust-on-workers-notes.md` `startup_time_ms`, backend and web Wasm sizes, and `cpuTime` from `wrangler tail` for one sync of the largest real feed (§3 ⚠️1, ⚠️2).
- [x] **Step 5: Commit** — `test: browser end-to-end journey; docs and deploy tooling`

---

## Spec coverage map

| Spec | Task(s) |
|---|---|
| §3 platform gaps, §5.3–5.4 ports, transactions, panics, lints | 1, 2 |
| §6.2 schema, §6.3 invariants | 1, 6, 8, 9 |
| §7.1–7.2 alarm cycle, item vs step errors, floors | 9 (queue part 6, sync/alerts 8) |
| §7.3 reconciliation | 6 |
| §7.4 expansion, anchor, budget, disabled sources | 5, 6, 13, 14 |
| §7.5 sync | 8 |
| §7.6 planning, dispatch, "already started" | 6, 9 |
| §7.7 inline API path | 13 |
| §7.8 watchdog | 9 |
| §8 time model | 3, 13, 14 |
| §9 ICS matrix | 4 |
| §10 channels | 7 |
| §11 security, WebAuthn checks | 10, 11, 12, 13, 14 |
| §12 HTTP surface | 1, 12, 13, 14, 17 |
| §13 web app, CSP | 15, 16 |
| §16 observability | 9 (logging), 14 (log route), 16 (Log page) |
| §17 build/deploy | 2, 15, 17 |
| §18 testing | every task; e2e 2, 17 |
| §20 measurements, notes | 2, 17 |
