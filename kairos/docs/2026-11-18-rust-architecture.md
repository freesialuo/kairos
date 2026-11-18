# Kairos — Personal Reminder Hub: Architecture Design (all-Rust)

Date: 2026-11-18 · Status: product scope approved by the user on 2026-11-17 (unchanged). Implementation re-based on an all-Rust stack on 2026-11-18: the user chose Rust for the backend **and** the frontend, picked Approach A (§4.2), and accepted Claude's defaults (§19). This document supersedes the TypeScript architecture in `2026-11-17-design.md`; the product behaviour it specifies is carried over unchanged unless §21 says otherwise.

## 1. Intent

A single-user, serverless personal reminder hub on Cloudflare Workers that runs on its own, with no always-on host and no need for anyone to have the site open.

- **Two event sources, plus manual entry:** (1) external ICS subscription feeds pulled on a per-source schedule (class timetables, work calendars, public holidays); (2) events pushed by scripts or services through an authenticated REST API; plus events created by hand in the web UI.
- **Reminders sent on its own:** the backend wakes itself before an event starts, **even when nobody is visiting the site**, and sends a reminder through Bark. Bark sits behind a channel interface, so a Telegram bot or another channel can be added later.
- **Management UI behind login:** a passkey-protected web app to view the merged calendar, manage feeds, configure notifications and time zone, and read the delivery log.
- **Nothing runs while idle:** no compute or storage writes between scheduled work items, apart from a 6-hourly watchdog (§7.8).
- **It is an experiment in running a complete product in Rust on Workers.** Every piece of application code (Worker, Durable Object, web UI, tests, build tooling) is Rust. Where Rust-on-Workers works well and where it hurts is a result worth recording (§20).

## 2. Goals / Non-goals / Anti-goals

**Goals**
- Merge ICS, API, and manual events into one timeline, and remind at the right time across recurring series, DST transitions, and mixed time zones.
- Run background scheduling independently of client visits. Sleep when idle; wake only when work is due.
- Protect the web UI with multiple WebAuthn passkeys, with a rate-limited recovery code for first-time setup and lost devices.
- Stay within the Cloudflare Workers Free plan under normal single-user load.
- All application code in Rust. The only non-Rust pieces are third-party tools that cannot be avoided (`wrangler`, a browser) and one five-line `boot.js` that loads the web app's Wasm (§13.2).
- Most behaviour testable natively with `cargo test`, without a Workers runtime (§18).

**Non-goals (v1)**
- Multiple users, accounts, or shared calendars.
- Publishing an outbound ICS / CalDAV feed.
- Writing back to external feeds (pull-only).
- Telegram bot delivery (the channel interface leaves room for it).
- Conversational bot features (snooze from a chat, natural-language event creation).
- Native mobile or desktop apps.
- `RDATE` support (detected and reported as a warning; see §9).
- Server-side rendering of the web UI (rejected in §4.2).
- Verifying authenticator attestation (which vendor made a key); see §11.2.

**Anti-goals (failures even if the system nominally runs)**
- **Silent failure:** dropped notifications, stalled syncs, truncated recurrence expansion, broken feeds (such as HTML error pages served with HTTP 200), or a lost alarm that the user never finds out about.
- **Duplicate spam:** the same reminder pushed repeatedly.
- **Wrong times:** reminders early, late, or on the wrong day because of time zones, DST shifts, or mishandled `RRULE` / `EXDATE` / `RECURRENCE-ID` / cancelled instances.
- **Quota surprises:** hot loops, chatty polling, or unbounded CPU or writes that exhaust Free plan limits or cause charges.
- **Home-made crypto mistakes:** the passkey verifier is our own code (§11.2), so a check silently skipped would be a login bypass.

## 3. Platform Constraints (verified against official docs and sources, 2026-11)

**Workers and Durable Objects**
- **Workers Free:** 10 ms CPU per invocation (time spent awaiting network or storage I/O does not count); 100,000 requests/day; ≤ 50 subrequests per invocation; static asset requests are free and unlimited.
- **Worker size:** 64 MiB uncompressed on every plan; there is no longer a compressed-size limit. **Startup** (instantiating the module and running global scope) must finish within 1 s; `wrangler deploy` reports `startup_time_ms`.
- **SQLite-backed Durable Objects on Free:** 5 M rows read/day, 100 k rows written/day, 5 GB total. **Each `setAlarm()` is billed as one row written.**
- **Alarms:** at most one alarm per object. `alarm()` runs **at least once** (replays are possible). A failed invocation is retried with exponential backoff starting at 2 s, at most 6 retries; after that the alarm is dropped. 15-minute wall-clock limit per invocation.
- **Atomicity:** each `sql.exec()` is atomic, and several `exec()` calls with no `await` between them are committed together (crash-atomic). They are **not rolled back** if the code fails half-way; only `ctx.storage.transactionSync(callback)` rolls back when the callback throws. **Output gate:** outgoing network messages are held until all earlier writes are durable.
- **SQL restrictions:** `exec()` cannot run `BEGIN`/`SAVEPOINT`. Cursors must be fully consumed before the next `await`.
- **KV is excluded:** eventually consistent, 1,000 writes/day on Free, no range queries.
- **WebAuthn:** credentials are bound to the RP ID. Changing the hostname makes every existing key unusable.

**Rust on Workers (`workers-rs`)**
- **SDK:** the `worker` crate (0.8.7 at the time of writing) supports SQLite-backed Durable Objects (`state.storage().sql()` → `exec`, `exec_raw`, `database_size`), the `alarm()` handler, `set_alarm`/`get_alarm`, `#[event(fetch)]`, and `#[event(scheduled)]`. The build tool `worker-build` produces the JS shim, runs `wasm-bindgen` and `wasm-opt`.
- **Gap — no transaction API:** `SqlStorage` in `worker` 0.8.7 exposes no `transactionSync`. The Hub binds it itself (§5.4).
- **Panics:** the default `panic=abort` turns a panic into a Wasm trap. `worker-build --panic-unwind` (uses a nightly toolchain and rebuilds `std`) makes panics catchable with `catch_unwind`, keeps the instance usable, and reinitialises the instance after a hard abort (out-of-memory, stack overflow). After a reinitialisation, the Durable Object's in-memory state is gone; the Hub keeps none that matters.
- **No system clock in Wasm:** `std::time::SystemTime::now()` and `Instant::now()` panic on `wasm32-unknown-unknown`, and so do library helpers built on them (`jiff::Timestamp::now()` without its `js` feature, `chrono::Utc::now()`). Time always comes from the injected clock (§5.3).
- **Randomness:** `getrandom` needs its JavaScript backend (`crypto.getRandomValues`) enabled for `wasm32-unknown-unknown`.
- **WebAuthn libraries:** `webauthn-rs` 0.5 has a hard OpenSSL dependency and does not compile to Wasm. 0.6.2 compiles but panics in `finish_passkey_registration` (it reads `SystemTime`). `passkey-server` is very young. The Hub therefore implements relying-party verification itself on pure-Rust crypto crates (§11.2).

**⚠️ To verify on deploy**
1. Whether the Durable Object CPU limit on Free is also 10 ms per `alarm()` / request. The architecture keeps every unit of work small (§7.1), so the only step that cannot be split is parsing one large feed. If that alone exceeds the limit, the Paid plan ($5/month) fixes it with no code change. Rust parses faster than the earlier JS design would have, so this risk is lower.
2. Startup time of the backend Wasm, which bundles two time-zone databases (§8).
3. That the raw storage handle needed for the `transactionSync` binding is reachable from `worker::State` (§5.4).

## 4. Approaches

### 4.1 Platform (decided 2026-11-17, unchanged)

| Dimension | D1 + Cron Triggers | **One Durable Object (SQLite) + alarm** ✅ | KV only (rejected) |
|---|---|---|---|
| Idle overhead | Wakes every minute (~1,440/day) | Sleeps until the next scheduled moment | — |
| Precision | 1 minute | 1 second | — |
| Concurrency / duplicates | Cron runs can overlap; needs locking | Single-threaded; only alarm replay to guard against | Eventual consistency; misses and duplicates likely |
| Why rejected | Idle cost, locking | — | 1 k/day write cap, no range queries, weak consistency |

One user maps to one named Durable Object (`id_from_name("main")`). SQLite data, auth state, and one self-arming alarm in a single single-threaded object give strong consistency, second-level precision, and zero idle wake-ups.

### 4.2 Language and stack (decided 2026-11-18)

| | **A. JSON API + Rust single-page app** ✅ | B. Server-side rendering (Leptos SSR in the Worker) | C. TS shell + Rust core (rejected by the user) |
|---|---|---|---|
| Shape | Hub serves JSON; Leptos app compiled to Wasm and served as static assets | Worker renders pages from DO data | DO, auth, routing in TS; parsing and expansion in Rust Wasm |
| CPU per page view | None on the Worker (static assets are free) | Rendering inside the 10 ms budget, plus a DO round trip | — |
| Script API | Same JSON API | Still needs a separate REST API | — |
| Maturity on workers-rs | Ordinary SPA + ordinary JSON | Few published examples; highest risk | Highest maturity |
| Why chosen / rejected | Maps one-to-one onto the approved architecture; risk stays in the product, not the rendering | Risk concentrated in rendering, which is not the product's core | Not all-Rust; contradicts the point of the experiment |

**Framework:** Leptos (client-side rendering), the most active Rust web UI framework with the most examples. Dioxus is the fallback if Leptos blocks progress; Yew is less active.

---

## 5. System Architecture

### 5.1 Context and components

```
                ┌───────────────┐        ┌──────────────────┐
  Browser ─────►│               │        │ ICS feed servers │◄── HTTPS GET (conditional)
  (Leptos SPA)  │  Entry Worker │        └──────────────────┘
                │  (Rust, Wasm) │                 ▲
  Scripts ─────►│  fetch +      │        ┌────────┴──────────┐
  (Bearer)      │  scheduled    │        │ Hub Durable Object│──► Bark server (HTTPS POST /push)
                └─┬───────┬──┬──┘        │ (Rust, Wasm;      │
       static     │       │  └─cron 6h──►│  instance "main") │
       assets ◄───┘       └─/api/*──────►│  ├ router (core)  │
       (Leptos dist,        /auth/*      │  ├ auth / webauthn│
        SPA fallback,                    │  ├ domain         │
        _headers CSP)                    │  ├ scheduler ⏰    │
                                         │  └ SQLite         │
                                         └───────────────────┘
```

- **Entry Worker (stateless):** with `run_worker_first: ["/api/*", "/auth/*"]`, only API and auth paths reach Worker code; they are forwarded unchanged to the Hub. Everything else is served from static assets with SPA fallback, without running Worker code. Its `scheduled` handler is the watchdog (§7.8).
- **Hub (stateful):** owns all data, enforces all authentication, runs all background work. Entry points: `fetch()` (HTTP) and `alarm()` (scheduler).

### 5.2 Cargo workspace

| Crate | Target | Contents | Depends on |
|---|---|---|---|
| `kairos-core` | native + `wasm32-unknown-unknown` | All behaviour: domain, scheduling, sync, auth, WebAuthn verification, **the HTTP router and handlers**. Talks to the outside world only through ports (§5.3). | `kairos-types`, pure-Rust crates (§5.5) |
| `kairos-worker` | `wasm32-unknown-unknown` (`cdylib`) | Entry Worker and the `Hub` Durable Object; adapters that implement the ports on Workers APIs; request/response conversion. Thin: no business rules. | `kairos-core`, `worker` |
| `kairos-web` | `wasm32-unknown-unknown` | Leptos SPA built with Trunk | `kairos-types`, `leptos`, `leptos_router`, `web-sys`, `gloo-net` |
| `kairos-types` | both | Serde DTOs for every request and response body, and shared validation limits (lengths, ranges, regexes) | `serde` |
| `xtask` | native | `cargo xtask build | test | e2e | deploy` (§17) | — |
| `e2e` | native | Browser end-to-end tests (§18) | `chromiumoxide`, a mock Bark and feed server |

`kairos-types` is the contract between the SPA, the Hub, and scripts: a field changed on one side fails to compile on the other.

### 5.3 Ports (the boundary between `kairos-core` and the platform)

```rust
pub trait Db {                                    // synchronous, like DO SQLite
    fn exec(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, DbError>;
    fn transaction<T>(&self, f: impl FnOnce(&Self) -> Result<T, CoreError>) -> Result<T, CoreError>;
}
pub trait Clock   { fn now_ms(&self) -> i64; }    // the only source of time
pub trait Entropy { fn fill(&self, buf: &mut [u8]); }
pub trait Http    { async fn send(&self, req: OutRequest) -> Result<OutResponse, NetError>; }
pub trait Alarm   { async fn get(&self) -> Option<i64>; async fn set(&self, at_ms: i64); }
pub trait Commit  { async fn commit_point(&self); }   // forces pending writes durable (§6.3, Invariant 2)
```

- **Values:** `Value = Null | Integer(i64) | Real(f64) | Text(String) | Blob(Vec<u8>)`. `Row` is a column-name → `Value` map with typed getters. `exec` always returns fully materialised rows, which satisfies the "consume cursors before `await`" rule by construction.
- **Async:** ports use `async fn` in traits with non-`Send` futures (Workers is single-threaded). `kairos-core` is generic over the ports (`Hub<P: Ports>`), so there is no `dyn` and no boxing.
- **`OutRequest`** carries a `max_body_bytes`. The adapter streams the body and aborts past the cap, so a 5 MB guard (§7.5) is enforced before the whole body is in memory.
- **`Config`** (not a port): `rp_id`, `origin`, the SHA-256 digest of the recovery code, the session secret, and feature switches (`allow_http_urls` for tests only).
- **Production adapters** (`kairos-worker`): `Db` → `SqlStorage` + the `transactionSync` binding; `Clock` → `Date.now()`; `Entropy` → `getrandom`; `Http` → `worker::Fetch`; `Alarm` → `storage.get_alarm/set_alarm`; `Commit` → `ctx.storage.sync()` (bound like `transactionSync` if the SDK does not expose it).
- **Test adapters** (in a separate `kairos-testkit` crate, which also holds the integration tests, to avoid a dev-dependency cycle): `Db` → in-memory `rusqlite` with real transactions; `FakeClock`; seeded `Entropy`; scripted `FakeHttp` that records every request; `FakeAlarm`; `Commit` → no-op.

### 5.4 Runtime model

- **One instance.** All requests and alarms are serialised by the Durable Object's input gate, so the code never races itself.
- **Migrations** run once per cold start, before the first request or alarm is handled (forward-only, version in `settings` key `meta:schema_version`).
- **Transactions:** writes that must change together (an event and its reminders, a queue item and its result) go through `Db::transaction`. The Workers adapter calls `ctx.storage.transactionSync(callback)` through a small `wasm-bindgen` extern. The callback is a Rust closure; an `Err` is turned into a thrown JS error, so the runtime rolls back. Because `Db` is synchronous, a transaction cannot contain an `await`; the compiler enforces it.
  - **Fallback** if the storage handle cannot be reached from `worker::State` (§3 ⚠️3): patch the `worker` crate with a `[patch]` entry that exposes it (and offer the change upstream). Last resort: rely on no-`await` crash-atomicity and write in "validate everything, then write" order.
- **`rearm()`** is the only function that calls `Alarm::set`. It skips the call when the target equals the current alarm, since every `setAlarm()` costs a row write.
- **Panics:** built with `--panic-unwind`. The `alarm()` handler wraps each step in `catch_unwind` (via `AssertUnwindSafe(..).catch_unwind()`), so a panic is handled like any other step error (§7.1). The `fetch()` handler turns a panic into a `500` with a generic message. Hard aborts reinitialise the instance; persisted state is unaffected.
- **Lints** that keep panics and wall-clock reads out of production paths: `clippy::unwrap_used`, `expect_used`, `panic`, `indexing_slicing` denied outside tests; `clippy::disallowed_methods` for `SystemTime::now`, `Instant::now`, `jiff::Timestamp::now`, `chrono::Utc::now`, `chrono::Local::now`. Release profile: `lto = true`, `codegen-units = 1`, `strip = true`, `overflow-checks = true` (an arithmetic bug becomes a caught panic, not a silently wrong time).

### 5.5 Crates chosen

| Concern | Crate(s) | Notes |
|---|---|---|
| Workers bindings | `worker`, `wasm-bindgen`, `js-sys`, `web-sys` | Entry Worker + DO |
| Routing (inside `kairos-core`) | `matchit` | Path matching only; handlers are plain functions over core `Request`/`Response` types |
| JSON | `serde`, `serde_json` | |
| ICS lines and components | `ical` (0.11) | Unfolding, property/parameter parsing, component tree. Semantics (§9) are ours. |
| Recurrence | `rrule` (0.14) | Used in *floating* time only (§7.4); brings `chrono` + `chrono-tz` |
| Time zones, civil time | `jiff` (bundled tzdb on Wasm) | DST resolution with `Disambiguation::Compatible` (§8) |
| Hashes, MAC, constant time | `sha2`, `hmac`, `subtle` | |
| Randomness | `getrandom` (JS backend) | |
| WebAuthn wire types | `webauthn-rs-proto` (pure serde) — or hand-written in `kairos-types` if its browser conversions do not fit Leptos | |
| CBOR / COSE | `ciborium`, `coset` | |
| Signatures | `p256` (ES256), `ed25519-dalek` (EdDSA), `rsa` (RS256 verify only) | Verification only; no private-key operations |
| Encoding | `base64` (URL-safe, no padding) | |
| Web UI | `leptos`, `leptos_router`, `gloo-net`, `web-sys` | Client-side rendering |
| Tests | `rusqlite` (bundled), `proptest`, `wasm-bindgen-test`, `chromiumoxide`, `tokio` (e2e only) | |

Exact versions are pinned in the implementation plan.

---

## 6. Data Architecture

### 6.1 Conventions

- **Instants:** UTC epoch milliseconds (`INTEGER`). They cross the JS boundary as `f64`, which is exact below 2^53; every value stored is far below that.
- **Wall-clock times:** `YYYY-MM-DDTHH:mm:ss` (timed) or `YYYY-MM-DD` (all-day), paired with `tzid`, which is an IANA zone, `'UTC'`, or `NULL` for floating time (interpreted in the user's time zone).
- **Booleans:** `INTEGER` 0/1. **JSON columns:** `TEXT`, always parsed with `serde_json` into typed structs.
- **Identity keys:**
  - `series_key`: `ics:<sourceId>:<uid>` · `api:<externalId>` · `manual:<eventId>`
  - `occ_key`: `<series_key>|<slot>`. `slot` is the occurrence's *original* wall-clock start for recurring series (the override's `RECURRENCE-ID`, which is the same value), or `single` for non-recurring events. An instance moved upstream therefore keeps its key.
  - reminder `key`: `<occ_key>|<leadMin>` (timed) or `<occ_key>|allday`.

### 6.2 Schema (migration 1)

```sql
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);          -- JSON values; 'meta:*' keys are internal
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
  initial_start_utc INTEGER,            -- exact API start when local time is ambiguous
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
```

Compared with the TypeScript design: `sources.alerted`, `sources.notify` and `passkeys.alg` are new; nothing else changed.

**`events.lead_minutes`:** `NULL` = inherit (§7.6); `'[]'` = **no reminders** for this event; a non-empty JSON array = these lead times.

**Settings keys:** `timezone` (default `'UTC'`; set to the browser zone during first-time setup), `defaultLeadMin` (15), `allDayTime` (`'09:00'`).
**Meta keys:** `meta:schema_version`, `meta:last_maintenance_at`, `meta:last_maintenance_error`, `meta:alarm_error_streak`, `meta:last_alarm_error`, `meta:last_watchdog_at`, `meta:recovery_locked_until`, `meta:webauthn_user_id` (16 random bytes, base64url).

The SQL is plain SQLite. Tests run it on `rusqlite`'s bundled SQLite; the adapter smoke test (§18) runs the migration and the main queries on the real Durable Object as well, to catch dialect differences.

### 6.3 Data roles and invariants

- **`events`** is the source of truth for what is scheduled.
- **`reminders`** is the *current schedule*, not a history. When an event moves, its reminder row is rewritten in place, even if it was already sent. **History lives in `deliveries`**, an append-only log of every send attempt, skip, and alert, kept for 90 days.
- **No `occurrences` table.** Occurrences are expanded when needed. Each reminder row carries `occ_start_utc`, so dispatch never expands anything.
- **`reconcile_queue`** lists series whose reminders must be recomputed. Bulk changes go through it so no single invocation does unbounded work.
- **Invariant 1:** every `pending` reminder belongs to an existing `events` row. Dispatch double-checks and deletes orphans without sending.
- **Invariant 2 (progress is recorded before the risky part):** every background step writes a *pessimistic trace* (the record it would leave if it failed: timestamp moved on, attempt counted, error text `interrupted`) and reaches a commit point **before** it starts work the platform can kill (network I/O, feed parsing, expansion). On completion the step overwrites the trace with the real outcome. A step that the runtime kills (CPU limit, memory limit, eviction), that panics, or that returns an error therefore leaves the same durable trace, and a replay does not repeat the same work. This rules out hot loops (§7.2).
  - Commit points: an `await` on network I/O is one (the output gate makes earlier writes durable first). Before pure-CPU work the step calls `Commit::commit_point()`.

---

## 7. Core Flows

### 7.1 Alarm cycle

Each `alarm()` runs these steps in order. Each step has a budget, writes its pessimistic trace first (Invariant 2), and is isolated: an `Err` **or a panic** in one step is caught (`catch_unwind`), logged with the step name, and does not stop the next step.

| # | Step | Budget per wake | Pessimistic trace written first | On error / panic / kill |
|---|---|---|---|---|
| 1 | **Dispatch** due reminders (`pending`, `next_attempt_at <= now`, oldest first) | ≤ 20 reminders | Per reminder, before any check: `attempts += 1`, `next_attempt_at = now + retry_delay(attempts)` (§7.6) | Counted as a failed send attempt; continue with the next reminder |
| 2 | **Reconcile queue**: recompute queued series (`not_before <= now`, oldest first) | ≤ 10 series | Per item: `attempts += 1`, `not_before = now + 2^attempts min`, then `commit_point()` | Item stays queued with its backoff; after 3 failed attempts it is dropped and the error is written to the series' `expand_warning` and the Log. A `catching_up` result is a success, not a failure (§7.4) |
| 3 | **Source alerts**: any source with `consecutive_failures >= 3 AND alerted = 0` | all (normally 0–1) | `alerted = 1` | Logged; the alert is not retried (the Sources page stays red) |
| 4 | **Sync** one due source | 1 source | `last_sync_at = now`, `consecutive_failures += 1`, `last_error = 'interrupted'` | The trace *is* the failure record (§7.5) |
| 5 | **Maintenance**, if due | 1 pass | `meta:last_maintenance_at = now − 23 h`, then `commit_point()` | `meta:last_maintenance_error` set; next try in 1 hour |
| 6 | **Re-arm**, always | — | — | — |

**Item-level vs step-level errors.** Steps 1, 2 and 4 work on items (a reminder, a queue item, a source). The item's trace is written *before anything else is done with that item*, so an error or panic after that point is **item-level**: the trace already records progress, and the step continues with the next item. Anything that fails outside an item's traced region (the query that selects due items, or writing a trace itself) is **step-level**: the step stops and the failure counts toward the error floor exactly like the top-level catch (§7.2). Item-level errors never raise the floor, so one broken feed cannot delay every reminder.

A top-level `catch_unwind` around the whole sequence is the last line of defence (§7.2). The `alarm()` handler always returns `Ok`, so the runtime's own retry (§3) only ever sees invocations that were killed, never ones that failed in code.

**Maintenance pass (daily):** queue recurring parent series and events entering the 60-day window with reason `roll`. Record `meta:last_window_roll_at` on a successful roll, so a delayed maintenance run covers the intervening days; delete `deliveries` and non-`pending` reminders older than 90 days; delete expired `sessions`; delete `auth_failures` older than 1 hour; set `meta:last_maintenance_at = now`.

### 7.2 Next-alarm computation and backoff

`compute_next_alarm(now)` takes the minimum of:
1. `MIN(next_attempt_at)` over `pending` reminders;
2. `MIN(not_before)` over `reconcile_queue`;
3. for each enabled source, `last_sync_at + interval_min` (or `now` if never synced);
4. `meta:last_maintenance_at + 24 h` (or `now` if never run).

`rearm()` sets `target = max(compute_next_alarm(now), floor)`:
- **Normal floor:** `now + 1 s`. This drains backlogs quickly without busy-waiting inside a single invocation.
- **Error floor:** if the top-level catch fired or any step failed at step level (§7.1) in this alarm, `meta:alarm_error_streak += 1` and the floor becomes `now + min(2^(streak − 1), 15)` minutes (1, 2, 4, 8, 15). A clean run resets the streak to 0. The streak and last error are shown in the Log under "Background errors".
- HTTP routes call `rearm()` with the normal floor after every mutation.

Together with Invariant 2, this bounds worst-case wake-ups: even a step that fails on every run makes durable progress or backs off, so a permanent bug costs about 4 wake-ups per hour, not 86,400 per day.

### 7.3 Reconciliation: inline vs queued

`reconcile_series(series, now)` expands the series over `[now, now + 60 days)`, plans reminders (§7.6), and diffs them against the series' existing reminder rows:

| Planned vs stored | Action |
|---|---|
| Planned, not stored | Insert `pending`, `next_attempt_at = fire_at` |
| Stored `pending`, `fire_at` changed | Update `fire_at`, `next_attempt_at`; reset `attempts`, `late`, `last_error` |
| Stored `sent` / `skipped` / `failed`, `fire_at` changed (event rescheduled) | Rewrite the row as `pending` with the new time (the earlier outcome stays in `deliveries`) |
| Stored `pending`, no longer planned | Delete (with a `deliveries` row `kind='reminder'`, `ok=0`, `error='withdrawn: event changed or removed'` only if `attempts > 0`, so retries in flight are not lost silently) |
| Stored finished row, no longer planned | Keep until the 90-day purge |

**When it runs:**
- **Inline**, inside the same request and transaction: single-series edits, meaning API upsert/delete and manual create/edit/delete. A reminder for a just-created event exists before the response returns.
- **Queued:** anything that can touch many series:
  - settings change: `timezone` / `allDayTime` → floating and all-day series; `defaultLeadMin` → all series;
  - source settings change (including `enabled` and `notify`): that source's series;
  - source sync: each changed, created, or pruned series;
  - daily window roll: recurring series and one-off events newly inside the 60-day horizon.

  The response returns immediately. The UI shows "Recomputing reminders… N left" from the queue length. The queue drains at 10 series per wake, about one wake per second.

### 7.4 Recurrence expansion

`expand_series(rows, from_utc, to_utc, user_tz, opts) -> ExpandResult { occurrences, anchor, status, warning }` takes a parent row plus its override rows.

**Algorithm**
1. Zone per row is `tzid.unwrap_or(user_tz)`.
2. The suppressed slot set is the parent's `exdates` (which, after parsing, include cancelled instances, §9) plus every override's `recurrence_id`.
3. **Non-recurring parent:** one occurrence, slot `single`.
4. **Recurring parent — floating expansion with `rrule`:**
   - Build an `rrule::RRuleSet` whose `DTSTART` is the parent's *wall-clock* start labelled as UTC (`rrule::Tz::UTC`). Expansion therefore happens in civil time with no DST at all, which is what RFC 5545 specifies (a weekly 09:00 event stays at 09:00).
   - `UNTIL` given in UTC (`Z`) is first converted to wall-clock time in the row's zone, then labelled UTC the same way. Date-only `DTSTART` uses midnight.
   - The set is built **without** `EXDATE`s: raw slots must be counted (they count toward `COUNT`, see the anchor), so suppression is applied by our code, not by the crate.
   - Iterate the raw slots lazily. For each: increment the raw index; skip it if suppressed; convert it with `wall_to_utc(slot, zone)` (§8); drop it if before `from_utc`; stop at `to_utc`, at the end of the rule, or at the budget.
   - Iteration starts at the **anchor** when it is usable, otherwise at `DTSTART`.
5. **Each override row:** one occurrence with slot = its `recurrence_id`, emitted independently. An override whose parent is missing from the feed still produces its occurrence.

**Anchor (resume point).** Expanding from `DTSTART` every time is unbounded: a daily series started 15 years ago needs about 5,500 iterations before reaching today. So:
- The anchor is `{ slot, index, hash }`. `slot` is the last raw slot whose start is `< now − 1 day`. `index` is its 0-based position in the raw slot sequence (EXDATE'd slots included). `hash` is the parent row's content hash.
- **Resuming:** rebuild the rule with `DTSTART := anchor.slot` and, if the rule has `COUNT`, `COUNT := COUNT − anchor.index`. `UNTIL`, `INTERVAL`, and `BY*` parts are unchanged. Because the anchor is a real occurrence, the rebased rule generates exactly the tail of the original sequence.
- The anchor is used only when `anchor.hash == row.hash`, the anchor's start is `≤ from_utc`, and the rule has no `BYSETPOS`. `BYSETPOS` rules always iterate from `DTSTART`, within the budget.
- Reconciliation persists the new anchor. It only moves forward. For a `complete` series it moves at most once per day (the daily roll); while a series is `catching_up`, every pass moves it.

**Budgets and status**
- **Budget:** at most 2,000 raw slots per call (`opts.budget`, overridable in tests; `opts.budget = ∞` disables the anchor too, for differential tests).
- **Catching up:** if the budget runs out before reaching `from_utc`, return status `catching_up` and an anchor at the furthest slot reached. Reconciliation saves that anchor and treats the pass as a **success**: in the same transaction it deletes the queue item and re-enqueues the series with `attempts = 0`, `not_before = now`, reason `catch_up`. The next wake (about 1 s later) continues from the new anchor. A series 15 years old at one slot per hour (~130 k slots) catches up in about 65 passes, roughly a minute.
- **Too dense:** if the budget runs out inside the window (for example a rule repeating every minute), return status `too_dense` with the occurrences found so far, and the warning `Recurrence too dense; only the first N occurrences in the window are scheduled`.
- **Crate limits:** `rrule`'s own validation limits (for example the maximum `INTERVAL` per frequency) are above anything a real feed uses; a rule the crate rejects is reported as a parse warning and the row is treated as non-recurring.
- **Surfacing:** a non-`complete` status is stored on the parent row and shown on the event, on its source, and in the Log.

**Disabled sources:** a disabled source stops syncing, its events are hidden from the calendar and from `GET /api/v1/events`, and its reminders are withdrawn through the queue. The rows are kept, so re-enabling it shows them again and re-queues the series before the next sync.

**Calendar view queries** use the same function with the requested range (≤ 92 days). For ranges before the anchor, expansion starts at `DTSTART` within the budget. If it is truncated, the response marks the series `truncated: true` and the UI shows a hint.

### 7.5 Source sync

1. **Pick:** the oldest due enabled source.
2. **Pessimistic trace** (Invariant 2): `last_sync_at = now`, `consecutive_failures += 1`, `last_error = 'interrupted before completion (CPU or memory limit?)'`.
3. **Fetch:** a conditional `GET` with `If-None-Match` / `If-Modified-Since`, `max_body_bytes = 5 MB`. The `await` is the commit point for the trace. `304` → success with `last_result = 'unchanged'`, no parsing.
4. **Guard:**
   - Size: reject if `Content-Length > 5 MB`; the adapter also aborts the stream at 5 MB.
   - Format: the parser must find a root `VCALENDAR`, so an HTML page served with 200 fails here.
   - Errors: any non-2xx status, network error, guard failure, `Err`, or panic is a failure.
5. **On success:**
   - In one transaction: upsert every parsed row as origin `ics`; prune rows of this source that are no longer in the feed (deleted series, and stale overrides of series that still exist); enqueue each changed, created, or pruned series (reason `sync`); store the new validators; `consecutive_failures = 0`, `alerted = 0`, `last_error = NULL`, `last_result = 'ok: N events'` followed by any parse warnings.
6. **On failure:** existing events and reminders are left untouched. `last_error` is overwritten with the real error; the counter already counts this failure.
7. **Alert:** step 3 of the alarm cycle sends one "sync failing" alert when `consecutive_failures` reaches 3 and `alerted = 0`. Because the alert is its own step, it also goes out when the failing syncs were killed by the platform and never reached their own error handling.

### 7.6 Reminder planning and dispatch

**Planning (pure):**
- **Muted → nothing planned:** an event from a source with `notify = 0` or from a disabled source, or an event whose `lead_minutes` is `'[]'`, gets no reminders at all (timed or all-day). Reconciliation then withdraws any pending ones (§7.3).
- **Timed occurrences:** one reminder per lead time. Lead times resolve in this order:
  1. the event's explicit `lead_minutes` (non-empty);
  2. the feed's `VALARM` minutes, if the source honors VALARM and any exist;
  3. the source's `default_lead_min`;
  4. the global `defaultLeadMin`.

  Duplicates are removed. `fire_at = occ_start − lead`.
- **All-day occurrences:** exactly one reminder at `allDayTime` on the event date in the effective zone. Lead times do not apply.

**"Already started"** (used by dispatch):
- **Timed occurrence:** `now > occ_start_utc + 2 min`. The 2-minute grace equals the late threshold, so a lead-0 reminder (`fire_at == occ_start`) is still sent, with the `HH:mm · now` body (§10).
- **All-day occurrence:** the event's last day is over in the effective zone, i.e. `now ≥ occ_start_utc + duration`. A 09:00 reminder for an all-day event is therefore never skipped just because the day began at midnight; after an outage it is still sent (late) during the day.

**Dispatch of one reminder** (the due query returns keys; each key is then handled as one item):
1. **Pessimistic trace, first:** read `attempts` (the *pre-trace* value, called `a` below); write `attempts = a + 1` and `next_attempt_at = now + retry_delay(a + 1)`, where `retry_delay(1) = 1 min`, `retry_delay(2) = 5 min`, `retry_delay(3) = 15 min`, and `retry_delay(n ≥ 4) = 15 min` as a safety value only (the outcome write below sets the final status). If this write fails, the step fails at step level (§7.1).
2. If the event row is missing → delete the reminder, never send (Invariant 1).
3. If `a >= 4` (all attempts used, the last one interrupted) → `failed`.
4. If the event already started → `skipped` when `a = 0`, else `failed`.
5. If no channel is configured → `failed`.
6. Send (the `await` makes the trace durable before the push leaves, thanks to the output gate).
7. Outcome: ok → `sent`, `sent_at = now`; failed with `a + 1 < 4` → stays `pending` (the trace already holds the retry time); failed with `a + 1 = 4` → `failed`. Every attempt appends a `deliveries` row.

**Reminder lifecycle:**

```
            planned
               │
               ▼
   ┌──────► pending ──send ok──────────────► sent
   │           │ │
   │           │ └─send failed (attempt n)──► pending (next_attempt_at = now + 1/5/15 min)
   │           │        └─after 4th failed attempt (1 initial + 3 retries)──► failed
   │           ├─event already started, attempts = 0──► skipped
   │           ├─event already started, attempts > 0──► failed
   │           ├─no channel configured──► failed
   │           └─event row missing──► (deleted, never sent)
   └──── rescheduled (any state, fire_at changed) ◄── reconcile
```

- **Late send:** if `now − fire_at > 2 min` while the event has not started, the message is sent with a `(late)` prefix and `late = 1`.
- **Accepted risk:** the only path to a duplicate is "push succeeded, then the invocation died before the outcome write". The replay then sees the trace (`attempts` counted, retry time set) and sends again after 1 minute. This is accepted as better than missing a reminder.

### 7.7 API upsert (inline path example)

```
Script ─PUT /api/v1/events/standup─► Entry Worker ─► Hub.fetch ─► kairos_core::router
  verify_bearer → validate(EventPayload) → db.transaction(|tx| {
     upsert_event(tx, 'api:standup') → changed?
     changed → reconcile_series(tx, 'api:standup', now)
  }) → rearm() → 200 { id, created }
```

### 7.8 Watchdog (new)

An alarm can still be lost: if the runtime kills the same alarm invocation 7 times in a row (out of memory, CPU limit), it stops retrying, and nothing would wake the Hub until the next web request.
- The entry Worker has a Cron Trigger every 6 hours (`0 */6 * * *`). Its `scheduled` handler sends an internal request to the Hub (`/internal/rearm`, a path the entry Worker never forwards from outside, so it is unreachable from the internet).
- The Hub reads its current alarm. If one is set, it does nothing (no write). If none is set, it runs `rearm()` and logs `alarm was missing; re-armed by watchdog` to Background errors. It writes `meta:last_watchdog_at` only when it had to act, so a healthy day costs 4 requests and 0 writes.
- One cron trigger out of the 5 the Free plan allows per account.

---

## 8. Time Model

- **Conversion:** `wall_to_utc(wall, zone)` uses `jiff` with its bundled IANA database: `civil::DateTime` → `to_zoned(tz)` with `Disambiguation::Compatible`. This is the RFC 5545 rule, and it equals the rule the earlier design specified by hand:
  - **Spring-forward gap:** the wall time moves **forward** by the gap, using the offset in force before the transition. New York 2026-03-08 02:30 → 03:30 EDT (07:30 UTC); Berlin 2026-03-29 02:30 → 03:30 CEST (01:30 UTC).
  - **Fall-back overlap:** the earlier instant. New York 2026-11-01 01:30 → 01:30 EDT (05:30 UTC); Berlin 2026-10-25 02:30 → 02:30 CEST (00:30 UTC).
- **Two time-zone databases:** `rrule` depends on `chrono-tz`, but it is only used in floating (UTC-labelled) time, so its database never decides an offset. Only `jiff` does. The cost is binary size and startup time (§3 ⚠️2).
- **Recurring events** iterate in wall-clock time and convert each instance separately.
- **Floating times** (`tzid NULL`) and **all-day events** follow the user time zone. Changing the time zone or the all-day time re-queues every affected series.
- **API times with an explicit offset** are stored as wall-clock time in `timezone ?? userTz`, with that zone made explicit. Their instant stays fixed if the user later changes the time zone. When the wall time alone cannot recover the supplied instant, such as the later half of a fall-back overlap, the row also stores `initial_start_utc`. Only the recurrence slot matching the original start uses that value. Other slots are resolved in the stored time zone using the rules above.
- **The browser never converts zones.** Calendar responses carry both the UTC instant and the wall-clock strings in the user's zone (`start_local`, `end_local`), so the web app needs no time-zone database.

## 9. ICS Support Matrix

Parsing: `ical` provides unfolded lines, properties, parameters, and the component tree. Interpreting them (this table) is `kairos-core::ics`.

| Feature | Handling |
|---|---|
| `DTSTART`/`DTEND`/`DURATION`, `TZID` (IANA), `Z` (UTC), floating | Supported |
| `TZID` not a valid IANA name (e.g. `China Standard Time`) | Treated as floating (user zone) + warning in `last_result` |
| `VTIMEZONE` blocks | Ignored; zones resolve by IANA name only |
| `VALUE=DATE` all-day | Supported; default duration 1 day |
| `RRULE` (every part `rrule` accepts), `EXDATE` (multi-valued and repeated) | Supported; expansion as in §7.4. A rule `rrule` rejects → warning, row treated as non-recurring |
| `RECURRENCE-ID` override | Separate row, keeps the original slot's `occ_key` |
| `RECURRENCE-ID` + `STATUS:CANCELLED` (one instance cancelled) | **Merged into the parent's `exdates`**; the slot is suppressed, not resurrected |
| `STATUS:CANCELLED` on the master | Whole series dropped, including its overrides |
| Cancelled override whose master is absent | Ignored |
| Override whose master is absent | Kept as a stand-alone occurrence |
| `RANGE=THISANDFUTURE` | Treated as a single-instance override |
| `VALARM` with relative `TRIGGER ≤ 0`, `RELATED=START` (or default) | Converted to lead minutes |
| `VALARM` absolute or `RELATED=END` | Ignored |
| `RDATE` | Not supported in v1; warning `RDATE ignored on "<summary>"` |
| Not a `VCALENDAR` (HTML error page, empty body), invalid UTF-8 | Sync failure; existing data untouched |

## 10. Notification Channels

- **Interface:** `trait Channel { async fn send(&self, msg: &Message) -> Result<(), ChannelError>; }`, implemented on top of the `Http` port. Scheduling and retry logic know nothing about the transport.
- **Bark (v1):**
  - Request: `POST <server>/push`, JSON `{ device_key, title, body, url, group: "reminders", level: "timeSensitive" }`. The server is `https://api.day.app` by default, or a self-hosted server.
  - Success means HTTP 200 **and** response `code == 200`.
- **Message format:**
  - Title: `⏰ <title>`.
  - Body: `HH:mm · in N min` or `HH:mm · now` for timed events, `Today · all day` for all-day events; then the location and the source name.
  - Late sends get the `(late) ` prefix.
  - Link: `<ORIGIN>/calendar?event=<id>`.
- **Source alert:** title `⚠️ Source sync failing`, body `<name>: <error ≤ 200 chars>`, link `<ORIGIN>/sources`.
- **Diagnostics:** Settings has "Send test notification", which logs a `kind='test'` delivery.

## 11. Security Architecture

### 11.1 Principals and credentials

| Principal | Credential | Can access |
|---|---|---|
| Owner (web) | Passkey → `full` session | Everything under `/api/admin/*` and `/auth/*` |
| Owner recovering | Recovery code → `recovery` session | Only passkey registration (and logout) |
| Script | Bearer token `kr_…` | `GET /api/v1/events` (all origins, read-only); `PUT`/`DELETE` of `api`-origin events only |
| Watchdog | Internal path, reachable only from the entry Worker | `/internal/rearm` |
| Anonymous | — | `GET /auth/state`, login and recovery endpoints, static assets |

### 11.2 Passkeys (WebAuthn) — in-house relying-party verification

**Configuration and policy (unchanged)**
- `RP_ID` and `ORIGIN` are fixed once at deployment, either a custom domain or `*.workers.dev`.
- Options: discoverable credentials (`residentKey: "required"`), so login needs no username; `userVerification: "preferred"`; `attestation: "none"`; `pubKeyCredParams` = ES256 (-7), EdDSA (-8), RS256 (-257); `excludeCredentials` = the stored keys for this `RP_ID`.
- User verification is **not required** in either ceremony, so a key without PIN or biometrics that can register can also log in. User *presence* is always required.
- Login only considers keys whose `rp_id` equals the current `RP_ID`. Keys with another `rp_id` are shown as **stale** in Settings.
- Key management: rename (`PATCH /api/admin/passkeys/:id { name }`); revoke (`DELETE /api/admin/passkeys/:id`); revoking the last valid key first returns `409 { error: "confirm_required", warning }`, and the UI repeats the request with `?confirm=1` after a confirmation dialog.

**Challenges:** 32 random bytes in an HMAC-SHA256-signed `__Host-chal` cookie, never in the database; payload `{ purpose: "reg" | "auth", challenge, exp }`; lifetime 2 minutes; cleared after use; compared in constant time.

**Registration verification** (subset of WebAuthn Level 2 §7.1; every numbered check has a negative test, §18):
1. `type == "public-key"`; `id == base64url(rawId)`; `rawId` ≤ 1,023 bytes.
2. `clientDataJSON`: `type == "webauthn.create"`; `challenge` equals the cookie's (purpose `reg`); `origin == ORIGIN` exactly; `crossOrigin` absent or `false`.
3. `attestationObject` is a CBOR map with `fmt`, `attStmt`, `authData`. `fmt` may be anything and `attStmt` is ignored: no attestation trust, as requested with `attestation: "none"`.
4. `authData`: `rpIdHash == SHA-256(RP_ID)`; flag `UP` set; flag `AT` set; `UV`, `BE`, `BS` read but not required; attested credential data parsed (AAGUID, credential-id length and id, COSE key); an extensions map (`ED` flag) is skipped over, not treated as corruption; no trailing bytes.
5. The credential id inside `authData` equals `rawId`.
6. The COSE key is one of: EC2 P-256 with `alg -7` (point must decode on the curve); OKP Ed25519 with `alg -8`; RSA with `alg -257` and a modulus of at least 2,048 bits. Anything else → `400 unsupported_algorithm`, nothing stored.
7. The credential id is not already stored → else `409`.
8. Store `id`, the COSE key bytes, `alg`, `counter = signCount`, `transports`, `rp_id`, and a default name.

**Authentication verification** (subset of §7.2):
1. The credential id is stored with `rp_id == RP_ID`.
2. `clientDataJSON`: `type == "webauthn.get"`; challenge (purpose `auth`), origin, `crossOrigin` as above.
3. `authData`: `rpIdHash`, `UP` set.
4. `userHandle`, if present, equals `meta:webauthn_user_id`.
5. **Signature first:** over `authData || SHA-256(clientDataJSON)` with the stored key. ES256: DER-encoded ECDSA; high-S signatures are normalised before verification (authenticators are not required to produce low-S). EdDSA: strict Ed25519 verification. RS256: RSASSA-PKCS1-v1_5 with SHA-256.
6. **Counter second:** if the stored or the new counter is non-zero and `new <= stored`, reject and log "possible cloned authenticator"; otherwise store the new counter and `last_used_at`.

**Why in-house is acceptable:** the scope is small (one user, no attestation, three algorithms, no extensions), the code is about 300–400 lines of pure functions in `kairos-core::webauthn`, it uses audited crypto crates for the actual signature math, and it is tested natively with a software authenticator that mints real ceremonies (§18). `rsa` is used for public-key verification only, which the known `rsa` timing advisory (private-key operations) does not affect.

**Browser side:** `kairos-web` calls `navigator.credentials.create/get` through `web-sys`, converting base64url ↔ `ArrayBuffer` in Rust.

### 11.3 Recovery code

- Set with `wrangler secret put RECOVERY_CODE`. The Hub keeps only its SHA-256 digest in memory and compares digests with `subtle::ConstantTimeEq`.
- **Uses:**
  1. **First-time setup:** while no valid key exists for `RP_ID`, the first registration needs a recovery session. This stops strangers from claiming a fresh deployment.
  2. **Lost device or domain change:** the recovery session can only register a new passkey. Registering upgrades it to `full`.
- **Rate limit:** 5 wrong attempts within 1 hour → locked for 1 hour (`429`).
- **Lifetime:** the code stays valid until the secret is rotated.

### 11.4 Sessions and CSRF

- **Session cookie:** a random 32-byte token in `__Host-sid` (`HttpOnly; Secure; SameSite=Strict; Path=/`). Only its SHA-256 hash is stored.
- **Expiry:** 30-day sliding window. `expires_at` is refreshed at most once per day, to limit writes.
- **Logout:** of the current session, or "log out all devices".
- **CSRF:** every state-changing request under `/auth/*` and `/api/admin/*` must carry `Origin == ORIGIN`, otherwise `403`. Bearer-token routes are exempt because they use no cookies.

### 11.5 API tokens

- Format: `kr_` + 32 random bytes in base64url.
- Shown once when created. Only the SHA-256 hash is stored, plus a 10-character prefix for display, the name, and `last_used_at`.
- Revocable at any time.

### 11.6 Threats and mitigations

| Threat | Mitigation |
|---|---|
| Stranger claims a fresh deployment | First passkey requires the recovery code |
| Brute-forcing the recovery code | 5 attempts/hour + 1-hour lockout; long random code recommended (`openssl rand -base64 32`) |
| Bug in the in-house WebAuthn verifier | Checks listed one by one (§11.2), each with a negative test; signature verified by audited crates; real-browser fixture from the end-to-end test |
| Stolen session cookie via XSS | `HttpOnly`; Leptos renders all feed and event text as text, and `inner_html` is banned (lint/grep in CI); strict CSP as an HTTP header (§13.2) |
| CSRF | `SameSite=Strict` + `Origin` check |
| Leaked API token | Hash-only storage, narrow scope, revocation, `last_used_at` visible |
| Malicious / huge feed | 5 MB streaming cap, format guard, bounded expansion, text-only rendering, `overflow-checks` |
| Feed URL pointing at internal services (SSRF) | Only `https://` URLs accepted (except in test builds); Workers cannot reach private networks |
| Watchdog path abused from outside | `/internal/*` is outside `run_worker_first`, so outside requests get the SPA, never the Hub |
| Test hooks shipped to production | `test-hooks` is a Cargo feature that only the `e2e` build enables; `xtask deploy` refuses to deploy a build that has it (§17) |
| Secrets in exports | Export never includes passkeys, sessions, tokens, or the clear Bark key |

## 12. HTTP Surface

All routing happens in `kairos-core::router` (`matchit` path tables per prefix), so every route is testable natively. Request and response bodies are `kairos-types` structs.

| Prefix | Auth | Routes |
|---|---|---|
| `/api/health` | none | `GET` → `{ ok: true }` |
| `/auth/*` | mixed | `GET state`, `POST recovery`, `POST passkey/register/options|verify`, `POST passkey/login/options|verify`, `POST logout`, `POST logout-all` |
| `/api/admin/*` | `full` session + Origin | `calendar`; `sources` CRUD + `sync`; `events` CRUD (manual) / delete (api), `POST events/:id/skip { slot }` (delete one occurrence of a recurring `manual` series by adding an EXDATE; not offered for `api` series, because the script's next full-replacement `PUT` would undo it — scripts send `exdates` instead); `settings`; `channel` + `test`; `passkeys` list/rename/delete; `tokens` list/create/delete; `log`; `queue` (length); `export.json`, `export.ics` |
| `/api/v1/*` | Bearer | `PUT /events/{external_id}`, `GET /events?from&to` (≤ 92 days), `DELETE /events/{external_id}` |
| `/internal/rearm` | entry Worker only | Watchdog (§7.8) |
| `/__test/*` | only with the `test-hooks` feature | `POST clock { offset_ms }`, `POST alarm` (run one alarm cycle now) |

**API v1 payload:**
- Fields: `title` (1–200), `start` (ISO 8601 date or date-time), `end` (optional; default = start for timed events, start + 1 day for all-day), `all_day`, `timezone` (IANA), `location` and `description` (≤ 2,000 each), `rrule`, `exdates` (optional, ≤ 200 values in the same form as `start`; only with `rrule`), `reminders` (≤ 5 integers in 0–10,080).
- `reminders` absent = inherit the defaults; `reminders: []` = **no reminders** for this event.
- `external_id` must match `^[A-Za-z0-9._:-]{1,200}$`.
- Validation errors return `400 { error, field }`. Unknown JSON fields are rejected (`deny_unknown_fields`).

**API v1 semantics (the contract for the user's scripts):**
- `PUT /events/{external_id}` is a **full replacement** (idempotent upsert): fields left out take their defaults, they do not keep old values. Response `200 { id, created, reminders_scheduled }`. Unchanged content (same hash) changes nothing and returns `created: false`.
- `GET /events?from&to` returns **expanded occurrences** in `[from, to)`, from all origins, sorted by start: `{ occurrences: [{ occ_key, origin, external_id?, source?, title, location?, all_day, start_utc, end_utc, start_local, end_local }], truncated: [series_key…] }`. `from`/`to` are ISO 8601; the range may not exceed 92 days.
- `DELETE /events/{external_id}` → `204`, or `404` if unknown. Its reminders are withdrawn inline.

## 13. Web Application

### 13.1 Structure

- **Stack:** Leptos (client-side rendering) + `leptos_router`, compiled to Wasm by Trunk, served as static assets. API calls via `gloo-net` with `kairos-types` DTOs. All UI copy is English, in one `strings.rs`. Weeks start on Monday.
- **Calendar layout** uses the server's `start_local` / `end_local` strings (§8); the browser does no zone math. The browser's zone is read once, at first-time setup, through `Intl.DateTimeFormat().resolvedOptions().timeZone` (`js-sys`).
- **Navigation guards:**
  - `setupRequired` and no session → `/setup` (step 1: enter the recovery code);
  - `recovery` scope → `/setup` (step 2: register a passkey);
  - signed out → `/login`; any `401` from the API → `/login`.
- **First-time setup** finishes by saving the browser's time zone to Settings.

| Page | Capabilities |
|---|---|
| Calendar | Month/week views, one color per source, `?event=<id>` deep link highlight; detail drawer with reminder statuses; edit/delete for `manual`, delete-only for `api`, read-only for `ics`; truncated-series hint |
| Sources | Add/edit/enable/disable/delete; interval, default lead, honor VALARM, **"Send reminders" switch** (`notify`; e.g. off for a public-holiday feed); `last_result` with warnings; failing sources in red; "Sync now" |
| Events | Create/edit/delete `manual` events. **Repeat presets:** none / daily / weekly on chosen weekdays / monthly (on the start's day of month) / yearly, every N, ending never / on a date / after N times, turned into an `RRULE`. Edits apply to the whole series; **"Delete this occurrence"** adds an EXDATE (`manual` only; `api` scripts send `exdates`); changing one occurrence's time is not supported in v1. Reminders: "Default" / custom lead times / **"No reminders"** (`'[]'`). Delete `api` events; `ics` read-only (an `ics` event is muted through its source's switch, because a sync would overwrite a per-event setting) |
| Settings | Time zone, default lead, all-day time ("Recomputing reminders… N left", polled every 2 s while the queue drains); Bark config + test; passkeys (add, rename, revoke with last-key confirmation, stale badges); API tokens (create once-visible, revoke); JSON / ICS export |
| Log | Failed reminders pinned on top; deliveries newest first; failing sources; expansion warnings; background errors (alarm error streak, maintenance error, watchdog actions) |

### 13.2 Loading and CSP

- Trunk is configured with `inject_scripts = false` and `filehash = false`. A static five-line `boot.js` (the only hand-written JS in the repository) imports the generated module and calls `init()` on the `.wasm`. This avoids Trunk's inline loader script, so the CSP needs no `'unsafe-inline'` for scripts.
- Headers come from a `_headers` file in the asset directory (Workers static assets), so they are real HTTP headers:
  - `Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'` (`'wasm-unsafe-eval'` is what allows the browser to compile the app's Wasm);
  - `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`;
  - `Cache-Control: no-cache` on `index.html`, `boot.js`, and the app's `.js`/`.wasm` (revalidated by ETag, since file names are not hashed).

## 14. Failure Modes and Resilience

| Failure | Detection | Behavior | Visible where |
|---|---|---|---|
| Bark down / bad key | Send result not ok | Retries +1/+5/+15 min, then `failed` | Log (pinned) |
| No channel configured | Channel lookup is empty | Reminder `failed` | Log |
| Feed unreachable / non-2xx / too big / not ICS | Sync guard | Data kept; failure counter; alert at 3 | Sources (red), Log, one push |
| Unknown TZID / RDATE / rule rejected by `rrule` | Parser | Best-effort + warning | Sources `last_result` |
| Cancelled single instance | Parser | Slot suppressed | — (correct behavior) |
| Ancient or dense recurrence | Expansion budget | `catching_up` (resumes) or `too_dense` (partial) | Event, Sources, Log |
| `Err` or panic inside one reminder / series / sync / maintenance | Per-step `catch_unwind` | Pessimistic trace stands, backoff applies | Log |
| `Err` or panic outside the steps | Top-level `catch_unwind` | Error floor 1 → 15 min backoff | Log → Background errors |
| Invocation killed (CPU, memory, eviction) | Runtime retry of `alarm()` | Replay finds the pessimistic traces, does different work, re-arms | Log (`interrupted`) |
| Hard Wasm abort | wasm-bindgen abort hook | Instance reinitialised on next call; as above | Log |
| Alarm lost after 7 killed attempts | Watchdog finds no alarm | Re-armed within 6 h | Log → Background errors |
| Alarm replay | At-least-once semantics | Status check prevents re-send; only death-between-send-and-write can duplicate | — |
| DO restart / deploy | Alarm persists in storage | Next alarm fires normally; migration on cold start | — |
| Domain changed | No key matches `RP_ID` | `setupRequired` → recovery flow | Login page, Settings (stale keys) |

## 15. Resource Budget (typical single-user load)

| Item | Estimate / day | Free limit |
|---|---|---|
| Alarm wakes: 3 sources × 48 syncs + 30 reminders + 1 maintenance + ~10 queue drains | ~185 | 100,000 requests (DO + Worker) |
| Watchdog | 4 requests, normally 0 writes | (same pool); 1 of 5 cron triggers |
| Web + API requests | < 1,000 | (same pool) |
| Rows written: sync bookkeeping (now 2 writes per sync: trace + outcome), upserts of changed events, reminder rows, deliveries, `setAlarm` | ~2,500–5,500 (most on days a feed changes) | 100,000 |
| Rows read | < 200,000 | 5,000,000 |
| Storage | < 50 MB | 5 GB |
| CPU per invocation | ≪ 10 ms except parsing one large feed | 10 ms (⚠️ §3) |
| Backend Wasm size | a few MB uncompressed (two tz databases, crypto, regex) | 64 MiB; startup ≤ 1 s (⚠️ §3) |

**Worst case under a permanent bug:** about 4 error wakes per hour, thanks to the backoff (§7.2).

## 16. Observability

- **Log page** (§13) is the primary place to look.
- **`wrangler tail --format json`** shows `cpuTime`, exceptions, and console output. Every caught error and panic is logged with `console_error!` together with the step name; the panic hook includes the panic message and location.
- **Queue length** (`/api/admin/queue`) and the expansion statuses are shown in the UI.

## 17. Build, Deployment and Configuration

**Toolchains**
- Stable Rust (pinned in `rust-toolchain.toml`) for `kairos-core`, `kairos-types`, `kairos-web`, `xtask`, `e2e`.
- `worker-build --release --panic-unwind` for `kairos-worker`; it uses a nightly toolchain, which is pinned to a dated nightly so builds are reproducible.
- Trunk for `kairos-web`. Node.js only to run `wrangler`.

**`cargo xtask`**
- `build`: `trunk build --release` (into `crates/kairos-web/dist`, which includes `boot.js` and `_headers`), then the worker build.
- `test`: `cargo test --workspace` natively; `wasm-bindgen-test` (under Node) for the pure `kairos-core` modules on `wasm32` (time, ICS, expansion, WebAuthn) to catch Wasm-only problems such as a stray `SystemTime::now()`; `cargo clippy -- -D warnings`.
- `e2e`: builds with `--features test-hooks`, starts `wrangler dev --env e2e` plus a local mock Bark and feed server, runs the `e2e` crate.
- `deploy`: rejects builds with `test-hooks`, runs `build`, then `wrangler deploy`, then prints `startup_time_ms`.

**Configuration**

| Item | Where | Notes |
|---|---|---|
| `main`, build command | `wrangler.toml`: `main = "build/worker/shim.mjs"`, `[build] command = "worker-build --release --panic-unwind"` | `[env.e2e]` uses a build command with `--features test-hooks` **and** its own `assets.run_worker_first = ["/api/*", "/auth/*", "/__test/*"]`, so the test routes reach the Hub only in that environment |
| `RP_ID`, `ORIGIN` | `wrangler.toml` vars | Fixed once; changing them triggers the recovery flow |
| `RECOVERY_CODE`, `SESSION_SECRET` | `wrangler secret put` | Rotating `SESSION_SECRET` invalidates in-flight challenges only; rotating `RECOVERY_CODE` invalidates the old code |
| Durable Object | binding `HUB` → class `Hub`; migration `v1` `new_sqlite_classes = ["Hub"]` | |
| Watchdog | `[triggers] crons = ["0 */6 * * *"]` | |
| Static assets | `assets.directory = "crates/kairos-web/dist"`, `not_found_handling = "single-page-application"`, `run_worker_first = ["/api/*", "/auth/*"]` | `_headers` for CSP |
| Schema | Forward-only migrations in `kairos-core`, applied on cold start | |
| Backups | Settings → export JSON / ICS | Manual |

## 18. Testing Strategy

The port design (§5.3) moves almost all testing out of the Workers runtime.

- **Pure unit tests (native):** time zones (New York and Berlin gaps and overlaps), ICS interpretation (one fixture per row of §9), expansion, planning, message formatting, validation.
- **Property tests (`proptest`):**
  - `wall_to_utc` round-trips for every wall time that exists, and moves forward by exactly the gap for times that do not;
  - **differential anchor test:** for generated rules (daily, weekly with `BYDAY` and `INTERVAL`, monthly `BYMONTHDAY=31`, yearly, with `COUNT`, `UNTIL`, `EXDATE`), anchored expansion equals full expansion from `DTSTART` (`budget = ∞`) over the same window.
- **Golden recurrence tests:** the RFC 5545 §3.8.5.3 examples, so the floating-expansion wrapper around `rrule` is checked against the standard, not only against itself.
- **Hub integration tests (native, `rusqlite` + fakes):** the whole HTTP surface through `kairos_core::router`, reconciliation, the queue, the dispatch state machine, alarm arming and backoff, sync, auth flows, the watchdog. Clock, HTTP, entropy, and alarm are fakes.
- **Hot-loop and kill tests:** for each alarm step, (a) a permanently failing and (b) a permanently panicking fake prove that the next alarm is `≥ now + 60 s` after the first error and that the trace was written; (c) a "killed" run (the fake stops the step after the trace, without its error handling) followed by a replay proves the replay does not redo the same work.
- **WebAuthn tests:** a software authenticator (dev-dependency, using the signing halves of `p256`, `ed25519-dalek`, `rsa`) mints registration and login ceremonies for all three algorithms; one negative test per numbered check in §11.2 (wrong origin, wrong challenge, wrong purpose, `crossOrigin: true`, wrong rpIdHash, `UP` clear, trailing bytes, extensions present, unsupported algorithm, small RSA key, replayed counter, tampered signature, unknown credential, stale `rp_id`).
- **Wasm-target tests:** the pure modules again on `wasm32` under `wasm-bindgen-test`.
- **Adapter smoke tests** against `wrangler dev`: migration runs on real DO SQLite; one transaction rolls back on error (proves the `transactionSync` binding); `commit_point` works; alarm set/get; the 5 MB streaming cap.
- **End-to-end (`e2e` crate):** `chromiumoxide` drives Chrome with a CDP virtual authenticator against `wrangler dev --env e2e`: setup with recovery code → register passkey → create event → fast-forward the clock via `/__test/clock` → run `/__test/alarm` → the mock Bark server receives the push → logout → passkey login. The registration and login payloads are saved as fixtures for the WebAuthn unit tests (a real-browser-shaped ceremony).
- **On deploy:** `startup_time_ms`, and the CPU time of one large-feed sync (§3).

## 19. Decisions

**Confirmed by the user**
- 2026-11-17: Bark only in v1; Telegram later, behind the channel interface.
- 2026-11-17: a custom domain or `workers.dev` are both fine, fixed once; if the domain ever changes, fall back to the recovery code and re-register keys.
- 2026-11-17: passkeys with multiple keys and a recovery-code fallback; single Durable Object + alarm; no KV.
- 2026-11-18: everything in Rust, including the frontend; it is an experiment, to be carried all the way.
- 2026-11-18: Approach A (JSON API + Leptos single-page app), with Claude's defaults below.
- 2026-11-18 (after the builder check): a per-source "Send reminders" switch plus "No reminders" per manual/API event; a disabled source stops syncing, is hidden, and has its reminders withdrawn.
- 2026-11-18 (after the builder check): manual events get repeat presets and "Delete this occurrence"; no per-occurrence editing in v1.
- 2026-11-18: project name **Kairos** (Greek for "the right moment"); directory `kairos/`; crates `kairos-*`; Worker `kairos`; API token prefix `kr_`. The Durable Object keeps the role name `Hub` / binding `HUB`.

**Defaults chosen by Claude (revisable)**
- Product (carried over): manual events in the web UI; 60-day reminder window; 90-day history; default lead 15 min; all-day reminders at 09:00 with no lead times; week starts Monday; English UI; sync every 30 min (minimum 15), 5 MB cap, alert after 3 consecutive failures; 1 initial attempt + 3 retries (+1/+5/+15 min); late threshold 2 min; skip if the event already started; API tokens read all events but write only `api` events; the recovery code gates first-time setup and stays valid until rotated; better a rare duplicate than a missed reminder; queue (10 series per wake), anchor + 2,000-slot budget, per-step isolation with 1 → 15 min backoff; UV preferred, not required; confirmation before revoking the last key; cancelled instances become EXDATEs; no `RDATE`.
- Rust stack: Leptos CSR (Dioxus as fallback); `kairos-core` behind ports, tested natively on `rusqlite`; in-house WebAuthn verification on RustCrypto crates (ES256, EdDSA, RS256; no attestation); `jiff` for zone conversion, `rrule` in floating time for recurrence, `ical` for parsing; `matchit` router inside `kairos-core`; `--panic-unwind` with per-step `catch_unwind`; strict lints against panics and wall-clock reads; `chromiumoxide` for end-to-end tests; `cargo xtask` as the build driver.
- New in this revision: pessimistic progress traces before risky work (Invariant 2); source alerts as their own step (`sources.alerted`); a 6-hourly watchdog cron for lost alarms; CSP as a real HTTP header via `_headers`; the browser does no zone math.

## 20. Left to the Builder / Open Risks

- **Verify early (first plan task, a spike):** the `transactionSync` and `sync()` bindings (§5.4); `--panic-unwind` building with the chosen crates; `getrandom` backend; `jiff` + `chrono-tz` startup time; Leptos + Trunk with `inject_scripts = false`; `_headers` applied to SPA-fallback responses.
- DO CPU limit on Free and the cost of parsing the largest real feed (§3 ⚠️1).
- Whether `webauthn-rs-proto`'s types fit both sides, or hand-written DTOs in `kairos-types` are simpler.
- `rrule` behaviour on edge rules versus RFC 5545 (covered by the golden tests; a mismatch is fixed in our wrapper or upstream).
- UI styling and component structure; Leptos bundle size (static assets are free, but first load is slower than a JS SPA).
- If a real feed relies on `RDATE`, it is a candidate for v1.1.
- **Experiment notes:** keep a short `docs/rust-on-workers-notes.md` during the build, recording what worked, what needed workarounds, and measured numbers (startup time, CPU per sync, Wasm sizes).

## 21. Revision Log

- **2026-11-17 — product scope approved** (Chinese discussion; translated to English).
- **2026-11-17 — architecture revision after plan review** (TypeScript design, `2026-11-17-design.md` §21): hot-loop prevention, DST gap rule, cancelled instances, expansion anchor and budget, retry count, bounded queue, passkey rename and last-key confirmation, consistent UV settings, schedule vs history, status line.
- **2026-11-18 — re-based on an all-Rust stack** (this document):
  1. Stack: Rust workspace (`kairos-core`, `kairos-worker`, `kairos-web`, `kairos-types`, `xtask`, `e2e`); Leptos SPA; ports for DB, clock, entropy, HTTP, alarm, commit (§4.2, §5).
  2. Platform gaps closed: own `transactionSync` binding; `--panic-unwind` with per-step `catch_unwind`; no wall-clock reads in Wasm (§3, §5.4).
  3. WebAuthn verification implemented in-house, with every check listed (§11.2); `passkeys.alg` column.
  4. Time zones via `jiff` (`Compatible`), recurrence via `rrule` in floating time (§7.4, §8).
  5. **Fixed a gap that the TypeScript design also had:** a sync killed by the CPU limit could not record its failure, so the runtime would replay it until the alarm was dropped. Invariant 2 now requires a pessimistic trace and a commit point *before* risky work (§6.3, §7.1, §7.5, §7.6), source alerts became their own step (`sources.alerted`), and a 6-hourly watchdog re-arms a lost alarm (§7.8).
  6. CSP moved from a meta tag to a real header, now including `frame-ancestors` and `'wasm-unsafe-eval'` (§13.2).
  7. Calendar responses carry wall-clock strings, so the web app needs no time-zone database (§8).
  8. Testing: almost everything native on `rusqlite`; property tests; kill tests; WebAuthn negative tests; `chromiumoxide` end-to-end (§18).
- **2026-11-18 — after the builder check:**
  1. "Already started" defined per event type with a 2-minute grace; all-day reminders and lead-0 reminders were otherwise always skipped, in the TypeScript design too (§7.6).
  2. Dispatch writes its trace before any check; item-level vs step-level errors, and step-level errors raise the error floor (§7.1, §7.2, §7.6).
  3. `catching_up` is a successful pass that re-queues with `attempts = 0`, so old dense series are no longer dropped by the queue backoff (§7.1, §7.4).
  4. The `e2e` environment routes `/__test/*` to the Worker (§17).
  5. Retry delays defined without an out-of-range index (§7.6).
  6. API v1 semantics written down: PUT is a full replacement, GET returns expanded occurrences, `reminders: []` means none, optional `exdates` (§12).
  7. User decisions: "Send reminders" per source, "No reminders" per event, disabled-source behaviour; repeat presets and "Delete this occurrence" for manual events (§6.2, §7.4, §7.6, §12, §13).
