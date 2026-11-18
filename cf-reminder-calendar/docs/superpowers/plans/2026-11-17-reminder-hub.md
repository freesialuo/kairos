# Personal Reminder Hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A single-user Cloudflare Worker that merges pulled ICS feeds and API-written events, sends Bark reminders from a self-scheduling Durable Object alarm, and offers a passkey-protected web UI.

**Architecture:** One Worker serves the static SPA and forwards `/api/*` and `/auth/*` to a single SQLite-backed Durable Object `Hub` (`idFromName("main")`). The Hub owns all data, all auth checks, and one alarm set to the earliest pending piece of work (reminder, retry, source sync, daily maintenance). Pure modules (time zones, ICS parsing, expansion, reminder planning, message formatting) are tested without the DO; DO-level behavior is tested with `@cloudflare/vitest-pool-workers`.

**Tech Stack:** TypeScript, Wrangler, Durable Objects (SQLite storage API + alarms), Hono (routing inside the DO), `ical.js` (ICS parsing, RRULE iteration), `@simplewebauthn/server` v13 + `@simplewebauthn/browser`, Vite + Preact (web), Vitest 4 + `@cloudflare/vitest-pool-workers` (`cloudflareTest` plugin), Playwright (one end-to-end smoke).

**Spec:** `docs/2026-11-17-design.md` (read it before starting any task).

## Global Constraints

- Never delete files with `rm`, `rmdir`, `unlink`, `git clean`, `find -delete`, or programmatic unlink; move to Trash (`trash <path>` or `mv <path> ~/.Trash/`).
- Must run on the Workers Free plan: 10 ms CPU per invocation, ≤ 50 subrequests per invocation. No KV, no D1, no Cron Triggers.
- Exactly one Durable Object instance: `env.HUB.idFromName("main")`.
- All timestamps stored and passed as UTC epoch milliseconds (`number`); wall-clock times as strings `YYYY-MM-DDTHH:mm:ss` (timed) or `YYYY-MM-DD` (all-day).
- Spec values (copy exactly): expansion window 60 days; delivery log and finished reminders kept 90 days; default lead 15 min; all-day reminder time `09:00`; source interval default 30 min, minimum 15 min; ICS size cap 5 MB (5 × 1024 × 1024 bytes); source alert after 3 consecutive failures; send retries after 1, 5, 15 minutes, then `failed`; recovery code: 5 wrong attempts per hour, then locked 1 hour; challenge cookie valid 2 minutes; session 30-day sliding expiry.
- Secrets (via `wrangler secret`): `RECOVERY_CODE`, `SESSION_SECRET`. Vars: `RP_ID`, `ORIGIN`. Fixed at deployment.
- Every DO entry point (`fetch`, `alarm`) that mutates data ends by calling `Hub.rearm()`; `alarm()` re-arms in `finally`.
- Time, outbound `fetch`, and the notification channel are injected through `Hub.deps` so tests never hit the network or the real clock.
- UI copy in English, collected in `web/src/strings.ts` (one place to translate later).
- Commit after every task with a conventional-commit message; no attribution lines.

## Review Focus

1. **DST transitions** — a weekly 09:00 event in `America/New_York` stays at 09:00 local across the March/November changes, and a nonexistent local time (02:30 on spring-forward day) shifts forward to 03:30. Pinned in Task 2 and Task 4.
2. **Unknown TZID in a feed** (e.g. Outlook's `China Standard Time` without a usable IANA name) — interpreted in the user time zone and reported as a warning on the source's last sync result, never silently. Pinned in Task 3 and Task 8.
3. **A feed URL that returns an HTML error page with HTTP 200** — treated as a failed sync; existing events and reminders for that source are left untouched. Pinned in Task 8.
4. **Alarm replay and stale reminders** — running the alarm twice does not resend a `sent` reminder, and a reminder whose event was deleted after scheduling is never sent. Pinned in Task 7.
5. **Changing the user time zone or all-day time in Settings** — every pending reminder of floating-time and all-day events is recomputed. Pinned in Task 12.

## Spec deviations (decided in this plan)

- No `occurrences` table: occurrences are expanded on demand (calendar view, reconciliation). Reminders carry `occ_start_utc`, which is everything dispatch needs. This removes a table that would have to be kept in sync.
- All-day events get exactly one reminder at the all-day time; lead times do not apply to them.
- VALARM support: only `TRIGGER` durations relative to the event start (`-PT15M`, `-P1D`, also `RELATED=START`); absolute and end-relative triggers are ignored.
- `RANGE=THISANDFUTURE` overrides are treated as single-instance overrides.
- Discoverable credentials (`residentKey: 'required'`) so login needs no username.
- Week starts on Monday in the calendar view.

---

## File Structure

```
cf-reminder-calendar/
  package.json  tsconfig.json  wrangler.jsonc  vitest.config.ts
  src/
    index.ts              Worker entry: route to Hub or ASSETS; re-export Hub
    env.ts                Env interface
    types.ts              shared domain types (below)
    hub.ts                Hub DO: deps, migrate, fetch → Hono app, alarm, rearm
    db/schema.ts          MIGRATIONS + migrate(sql)
    db/settings.ts        getSettings / putSettings / meta get/set
    time/zone.ts          wall-clock ↔ UTC with Intl
    ics/parse.ts          ICS text → ParsedIcs
    ics/expand.ts         series → occurrences in a range
    calendar/events.ts    event store (upsert/delete/list series)
    calendar/reminders.ts planReminders (pure) + reconcileSeries / reconcileAll
    notify/message.ts     formatReminder / formatSourceAlert
    notify/bark.ts        BarkChannel
    sched/dispatch.ts     dispatchDue
    sched/scheduler.ts    computeNextAlarm, runAlarm, runMaintenance
    sources/sync.ts       syncSource, pickDueSource
    auth/crypto.ts        randomToken, sha256Hex, hmac sign/verify, timingSafeEqual
    auth/cookies.ts       parseCookies, setCookie
    auth/session.ts       createSession, readSession, extend, revoke
    auth/recovery.ts      checkRecoveryCode with rate limit
    auth/passkey.ts       registration/login option+verify flows (injected verifiers)
    auth/tokens.ts        createToken, verifyBearer, list, revoke
    http/app.ts           buildApp(hub): Hono app, middleware (origin check, session)
    http/auth-routes.ts   /auth/*
    http/api-v1.ts        /api/v1/*
    http/admin.ts         /api/admin/*
    http/validate.ts      event payload validation
    export/backup.ts      exportJson / exportIcs
  test/                   mirrors src/ (*.test.ts) + fixtures/*.ics
  web/                    Vite + Preact SPA (index.html, src/…)
  e2e/smoke.spec.ts       Playwright with a CDP virtual authenticator
  README.md               deploy + operations
```

### Shared types (`src/types.ts`, created in Task 1, used everywhere)

```ts
export type Origin = 'ics' | 'api' | 'manual';
export type ReminderStatus = 'pending' | 'sent' | 'failed' | 'skipped';

export interface EventFields {
  title: string; description: string | null; location: string | null;
  allDay: boolean;
  startLocal: string;          // 'YYYY-MM-DDTHH:mm:ss' or 'YYYY-MM-DD'
  tzid: string | null;         // IANA zone, 'UTC', or null = floating (user zone)
  durationSec: number;
  rrule: string | null;        // RRULE value without 'RRULE:'
  exdates: string[];           // wall-clock strings in the event's zone
  leadMinutes: number[] | null;   // explicit (api/manual); null = fall through
}
export interface StoredEvent extends EventFields {
  id: number; origin: Origin; sourceId: number | null; seriesKey: string;
  uid: string | null; recurrenceId: string | null; externalId: string | null;
  valarmMinutes: number[]; hash: string; updatedAt: number;
}
export interface Occurrence { eventId: number; occKey: string; startUtc: number; endUtc: number; }
export interface Settings { timezone: string; defaultLeadMin: number; allDayTime: string; }
export interface SourceRow {
  id: number; name: string; url: string; color: string; enabled: boolean;
  intervalMin: number; defaultLeadMin: number | null; honorValarm: boolean;
  etag: string | null; lastModified: string | null; lastSyncAt: number | null;
  lastResult: string | null; lastError: string | null; consecutiveFailures: number;
}
export interface PlannedReminder {
  key: string; seriesKey: string; eventId: number; occStartUtc: number;
  leadMin: number | null; fireAt: number;
}
export interface NotifyMessage { title: string; body: string; url?: string; }
export type SendResult = { ok: true } | { ok: false; error: string };
export interface Channel { kind: string; send(msg: NotifyMessage): Promise<SendResult>; }
export interface HubDeps {
  now(): number;
  fetch: typeof fetch;
  channel(): Channel | null;   // null when no channel is configured
}
```

Series keys: `ics:<sourceId>:<uid>`, `api:<externalId>`, `manual:<eventId>`. Occurrence key: `<seriesKey>|<slot>` where `slot` is the occurrence's original wall-clock start (recurring), the override's `recurrenceId`, or the literal `single` for non-recurring events. Reminder key: `<occKey>|<leadMin>` or `<occKey>|allday`.

---

### Task 1: Scaffold, Worker routing, Hub with schema

**Files:**
- Create: `package.json`, `tsconfig.json`, `wrangler.jsonc`, `vitest.config.ts`, `.gitignore`, `src/index.ts`, `src/env.ts`, `src/types.ts`, `src/hub.ts`, `src/db/schema.ts`, `src/db/settings.ts`, `src/http/app.ts`
- Test: `test/hub.test.ts`

**Interfaces:**
- Produces: `class Hub extends DurableObject<Env>` with `sql: SqlStorage`, `deps: HubDeps`, `rearm(): Promise<void>` (stub in this task: no-op), `fetch(req)`, `alarm()` (stub). `migrate(sql: SqlStorage): void`. `getSettings(sql): Settings`, `putSettings(sql, patch: Partial<Settings>): void`, `getMeta(sql, key): string | null`, `setMeta(sql, key, value): void`. `buildApp(hub: Hub): Hono`. `Env { HUB: DurableObjectNamespace<Hub>; ASSETS: Fetcher; RP_ID: string; ORIGIN: string; RECOVERY_CODE: string; SESSION_SECRET: string }`.

- [ ] **Step 1: Scaffold**

`git init`; `npm i hono ical.js @simplewebauthn/server`; `npm i -D wrangler typescript vitest@^4.1.0 @cloudflare/vitest-pool-workers @cloudflare/workers-types`. `.gitignore`: `node_modules`, `.wrangler`, `web/dist`, `.dev.vars`.

`wrangler.jsonc`: `name: "reminder-hub"`, `main: "src/index.ts"`, `compatibility_date: "2026-11-01"`, `compatibility_flags: ["nodejs_compat"]`, `durable_objects.bindings: [{ name: "HUB", class_name: "Hub" }]`, `migrations: [{ tag: "v1", new_sqlite_classes: ["Hub"] }]`, `vars: { RP_ID: "localhost", ORIGIN: "http://localhost:8787" }`, `assets: { directory: "./web/dist", binding: "ASSETS", not_found_handling: "single-page-application", run_worker_first: ["/api/*", "/auth/*"] }`.

`vitest.config.ts`: `cloudflareTest({ wrangler: { configPath: "./wrangler.jsonc" }, miniflare: { bindings: { RECOVERY_CODE: "test-recovery-code", SESSION_SECRET: "test-session-secret" } } })`, `test.include: ["test/**/*.test.ts"]`. Create an empty `web/dist/.gitkeep` so the assets directory exists. Scripts: `test`, `typecheck` (`tsc --noEmit`), `dev` (`wrangler dev`), `deploy`.

- [ ] **Step 2: Write the failing tests**

```ts
// test/hub.test.ts
import { env } from 'cloudflare:workers';
import { runInDurableObject, createExecutionContext } from 'cloudflare:test';
import worker from '../src/index';

const hub = () => env.HUB.get(env.HUB.idFromName('main'));

it('routes /api/health to the Hub', async () => {
  const res = await worker.fetch(new Request('http://localhost/api/health'), env, createExecutionContext());
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ ok: true });
});

it('creates every table on first start and is idempotent', async () => {
  await runInDurableObject(hub(), (h: any) => {
    const names = h.sql.exec("SELECT name FROM sqlite_master WHERE type='table'").toArray().map((r: any) => r.name);
    for (const t of ['settings','sources','events','reminders','deliveries','channels','passkeys','sessions','api_tokens','auth_failures'])
      expect(names).toContain(t);
    expect(() => migrate(h.sql)).not.toThrow();
  });
});

it('returns default settings', async () => {
  await runInDurableObject(hub(), (h: any) => {
    expect(getSettings(h.sql)).toEqual({ timezone: 'UTC', defaultLeadMin: 15, allDayTime: '09:00' });
  });
});
```

- [ ] **Step 3: Run test to verify it fails**

Run: `npx vitest run test/hub.test.ts` — Expected: FAIL (modules missing).

- [ ] **Step 4: Implement**

`src/index.ts`: for paths starting `/api/` or `/auth/` → `env.HUB.get(env.HUB.idFromName('main')).fetch(req)`; else `env.ASSETS.fetch(req)`. `export { Hub } from './hub'`.

`src/hub.ts`: constructor runs `ctx.blockConcurrencyWhile(async () => migrate(this.sql))`; `deps` defaults to `{ now: () => Date.now(), fetch: (...a) => fetch(...a), channel: () => null }` (Task 6 replaces `channel`). `fetch(req)` → `buildApp(this).fetch(req)`. `GET /api/health` returns `{ ok: true }` in `src/http/app.ts`.

`src/db/schema.ts`: `MIGRATIONS: string[]`, applied in order inside `sql` with a `_migrations(version INTEGER NOT NULL)` table recording the applied count. Migration 1 is exactly:

```sql
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sources (id INTEGER PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, color TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1, interval_min INTEGER NOT NULL DEFAULT 30, default_lead_min INTEGER,
  honor_valarm INTEGER NOT NULL DEFAULT 1, etag TEXT, last_modified TEXT, last_sync_at INTEGER,
  last_result TEXT, last_error TEXT, consecutive_failures INTEGER NOT NULL DEFAULT 0);
CREATE TABLE events (id INTEGER PRIMARY KEY, origin TEXT NOT NULL, source_id INTEGER, series_key TEXT NOT NULL,
  uid TEXT, recurrence_id TEXT, external_id TEXT, title TEXT NOT NULL, description TEXT, location TEXT,
  all_day INTEGER NOT NULL, start_local TEXT NOT NULL, tzid TEXT, duration_sec INTEGER NOT NULL, rrule TEXT,
  exdates TEXT NOT NULL DEFAULT '[]', lead_minutes TEXT, valarm_minutes TEXT NOT NULL DEFAULT '[]',
  hash TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE UNIQUE INDEX events_identity ON events(series_key, COALESCE(recurrence_id, ''));
CREATE UNIQUE INDEX events_external ON events(external_id) WHERE external_id IS NOT NULL;
CREATE TABLE reminders (key TEXT PRIMARY KEY, series_key TEXT NOT NULL, event_id INTEGER NOT NULL,
  occ_start_utc INTEGER NOT NULL, lead_min INTEGER, fire_at INTEGER NOT NULL, next_attempt_at INTEGER NOT NULL,
  status TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT, sent_at INTEGER,
  late INTEGER NOT NULL DEFAULT 0);
CREATE INDEX reminders_due ON reminders(status, next_attempt_at);
CREATE INDEX reminders_series ON reminders(series_key);
CREATE TABLE deliveries (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, reminder_key TEXT, kind TEXT NOT NULL,
  channel TEXT NOT NULL, ok INTEGER NOT NULL, error TEXT, summary TEXT NOT NULL);
CREATE TABLE channels (kind TEXT PRIMARY KEY, config TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE passkeys (id TEXT PRIMARY KEY, public_key BLOB NOT NULL, counter INTEGER NOT NULL, transports TEXT,
  rp_id TEXT NOT NULL, name TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE sessions (id_hash TEXT PRIMARY KEY, scope TEXT NOT NULL, created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, last_extended_at INTEGER NOT NULL);
CREATE TABLE api_tokens (id INTEGER PRIMARY KEY, name TEXT NOT NULL, hash TEXT NOT NULL UNIQUE,
  prefix TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE auth_failures (at INTEGER NOT NULL);
```

`src/db/settings.ts`: settings rows hold JSON values; defaults `{ timezone: 'UTC', defaultLeadMin: 15, allDayTime: '09:00' }`. Meta keys (`last_maintenance_at`, `recovery_locked_until`, `webauthn_user_id`) live in the same table under a `meta:` prefix.

- [ ] **Step 5: Run tests** — `npm test` → PASS; `npm run typecheck` → no errors.

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: scaffold worker, hub durable object and schema"`

---

### Task 2: Time zone utilities

**Files:** Create `src/time/zone.ts`; Test `test/time/zone.test.ts`

**Interfaces:**
- Produces: `isValidZone(tz: string): boolean`; `wallToUtc(wall: string, tz: string): number` (wall is `YYYY-MM-DDTHH:mm:ss`; nonexistent times shift forward by the gap; ambiguous times take the earlier instant); `utcToWall(utc: number, tz: string): string`; `dateAtTimeToUtc(date: 'YYYY-MM-DD', hhmm: string, tz: string): number`; `addDaysToDate(date: string, days: number): string`.

- [ ] **Step 1: Write the failing tests**

```ts
it('converts wall time in a fixed-offset zone', () => {
  expect(wallToUtc('2026-11-17T09:00:00', 'Asia/Shanghai')).toBe(Date.UTC(2026, 10, 17, 1, 0, 0));
});
it('keeps 09:00 local across New York DST', () => {
  expect(wallToUtc('2026-03-06T09:00:00', 'America/New_York')).toBe(Date.UTC(2026, 2, 6, 14));
  expect(wallToUtc('2026-03-09T09:00:00', 'America/New_York')).toBe(Date.UTC(2026, 2, 9, 13));
});
it('shifts a nonexistent spring-forward time forward', () => {
  expect(utcToWall(wallToUtc('2026-03-08T02:30:00', 'America/New_York'), 'America/New_York')).toBe('2026-03-08T03:30:00');
});
it('takes the earlier instant for an ambiguous fall-back time', () => {
  expect(wallToUtc('2026-11-01T01:30:00', 'America/New_York')).toBe(Date.UTC(2026, 10, 1, 5, 30));
});
it('validates zones', () => {
  expect(isValidZone('Europe/Berlin')).toBe(true);
  expect(isValidZone('China Standard Time')).toBe(false);
});
it('computes all-day reminder instant', () => {
  expect(dateAtTimeToUtc('2026-11-17', '09:00', 'Asia/Shanghai')).toBe(Date.UTC(2026, 10, 17, 1));
});
```

- [ ] **Step 2: Run** `npx vitest run test/time/zone.test.ts` — Expected: FAIL.
- [ ] **Step 3: Implement** with `Intl.DateTimeFormat(…, { timeZone, hourCycle: 'h23', year…second })` offset lookup: guess `utc = Date.UTC(wall)`, compute the zone offset at the guess, correct once, re-check; if the result's wall clock differs (gap), add the gap. Cache one formatter per zone in a `Map`.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `feat: add time zone conversion utilities`

---

### Task 3: ICS parsing

**Files:** Create `src/ics/parse.ts`; Test `test/ics/parse.test.ts`, fixtures `test/fixtures/{weekly-ny.ics,override.ics,allday-valarm.ics,unknown-tzid.ics,cancelled.ics}`

**Interfaces:**
- Consumes: `isValidZone` (Task 2).
- Produces:
```ts
export interface ParsedIcsEvent extends Omit<EventFields, 'leadMinutes'> {
  uid: string; recurrenceId: string | null; valarmMinutes: number[];
}
export interface ParsedIcs { events: ParsedIcsEvent[]; warnings: string[]; }
export class IcsFormatError extends Error {}
export function parseIcs(text: string): ParsedIcs;   // throws IcsFormatError if not a VCALENDAR
```

Rules: wall-clock strings come from the ICAL.Time fields (`year…second`), never from `toJSDate()`. `Z` times → `tzid: 'UTC'`. A TZID that `isValidZone` rejects → `tzid: null` and a warning `Unknown TZID "<id>" on "<summary>"; using your time zone`. `VALUE=DATE` → `allDay: true`, `startLocal: 'YYYY-MM-DD'`, duration in whole days (default 1 day). Duration from DTEND or DURATION, default 0 for timed. `STATUS:CANCELLED` events are dropped. RECURRENCE-ID wall-clock string goes in `recurrenceId`. VALARM: only `TRIGGER` durations relative to START that are ≤ 0 → minutes before (`-PT15M` → 15, `-P1D` → 1440). EXDATE values (multi-valued and repeated properties) → wall-clock strings.

- [ ] **Step 1: Write the failing tests** (one per fixture)

```ts
it('parses a weekly TZID event', () => {
  const { events, warnings } = parseIcs(fixture('weekly-ny.ics'));
  expect(warnings).toEqual([]);
  expect(events[0]).toMatchObject({ uid: 'w1', tzid: 'America/New_York', startLocal: '2026-03-02T09:00:00',
    durationSec: 3600, rrule: 'FREQ=WEEKLY;COUNT=4', exdates: ['2026-03-16T09:00:00'], allDay: false });
});
it('keeps an override as its own row with recurrenceId', () => {
  const { events } = parseIcs(fixture('override.ics'));
  expect(events.find(e => e.recurrenceId)).toMatchObject({ uid: 'o1', recurrenceId: '2026-03-09T09:00:00', startLocal: '2026-03-09T11:00:00' });
});
it('parses all-day events and relative VALARMs', () => {
  const e = parseIcs(fixture('allday-valarm.ics')).events[0];
  expect(e).toMatchObject({ allDay: true, startLocal: '2026-12-25', durationSec: 86400, valarmMinutes: [1440, 15] });
});
it('falls back and warns on an unknown TZID', () => {
  const r = parseIcs(fixture('unknown-tzid.ics'));
  expect(r.events[0].tzid).toBeNull();
  expect(r.warnings[0]).toContain('China Standard Time');
});
it('drops cancelled events', () => { expect(parseIcs(fixture('cancelled.ics')).events).toEqual([]); });
it('rejects non-calendar input', () => {
  expect(() => parseIcs('<!doctype html><html>error</html>')).toThrow(IcsFormatError);
});
```
(`fixture(name)` reads via `import.meta.glob('../fixtures/*.ics', { query: '?raw', eager: true })`.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** with `ICAL.parse` + `new ICAL.Component(jcal)`; wrap any parse error in `IcsFormatError`; require root name `vcalendar`. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: parse ICS feeds into normalized events`

---

### Task 4: Recurrence expansion

**Files:** Create `src/ics/expand.ts`; Test `test/ics/expand.test.ts`

**Interfaces:**
- Consumes: `wallToUtc`, `dateAtTimeToUtc`, `addDaysToDate` (Task 2); `StoredEvent`, `Occurrence`.
- Produces: `expandSeries(rows: StoredEvent[], fromUtc: number, toUtc: number, userTz: string): Occurrence[]` — `rows` are all rows of one series (parent + overrides); returns occurrences whose `startUtc` is in `[fromUtc, toUtc)`, sorted by `startUtc`.

Algorithm: the zone for a row is `row.tzid ?? userTz`. Non-recurring parent → one occurrence, slot `single`. Recurring parent → iterate `ICAL.Recur.fromString(rrule).iterator(ICAL.Time.fromDateTimeString/fromDateString(startLocal))` over **floating** wall-clock times; convert each with `wallToUtc` (all-day: midnight via `dateAtTimeToUtc(date,'00:00',zone)`); skip slots in `exdates` or in the set of override `recurrenceId`s; stop when `startUtc >= toUtc`; hard cap 5,000 iterations per series. Each override row → one occurrence with slot = its `recurrenceId`. `endUtc = startUtc + durationSec*1000` (all-day: midnight of `addDaysToDate(start, days)`).

- [ ] **Step 1: Write the failing tests**

```ts
const range = [Date.UTC(2026, 2, 1), Date.UTC(2026, 3, 1)] as const;
it('expands weekly across DST keeping 09:00 local and honoring EXDATE', () => {
  const occ = expandSeries([weeklyNy], ...range, 'UTC');  // weeklyNy: start 2026-03-02T09:00, COUNT=4, exdate 03-16
  expect(occ.map(o => o.startUtc)).toEqual([Date.UTC(2026,2,2,14), Date.UTC(2026,2,9,13), Date.UTC(2026,2,23,13)]);
  expect(occ[1].occKey).toBe('ics:1:w1|2026-03-09T09:00:00');
});
it('replaces an overridden slot with the override row, same occKey', () => {
  const occ = expandSeries([parent, override], ...range, 'UTC');  // override moves 03-09 09:00 → 11:00
  const o = occ.find(o => o.occKey === 'ics:1:o1|2026-03-09T09:00:00')!;
  expect(o.startUtc).toBe(Date.UTC(2026,2,9,15));
  expect(occ.filter(o => o.occKey.endsWith('2026-03-09T09:00:00'))).toHaveLength(1);
});
it('interprets floating times in the user zone', () => {
  expect(expandSeries([{ ...single, tzid: null }], ...range, 'Asia/Shanghai')[0].startUtc).toBe(Date.UTC(2026,2,10,1));
});
it('uses slot "single" for non-recurring events and respects the range', () => {
  expect(expandSeries([single], ...range, 'UTC')[0].occKey).toBe(`${single.seriesKey}|single`);
  expect(expandSeries([single], Date.UTC(2027,0,1), Date.UTC(2027,1,1), 'UTC')).toEqual([]);
});
```
(Build `StoredEvent` literals with a `makeEvent(partial)` helper in `test/helpers.ts`, reused by later tasks; defaults: `tzid: 'UTC'`, `durationSec: 3600`, no rrule. `single` here = `makeEvent({ seriesKey: 'manual:1', startLocal: '2026-03-10T09:00:00' })`.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: expand recurring series into occurrences`

---

### Task 5: Event store and reminder reconciliation

**Files:** Create `src/calendar/events.ts`, `src/calendar/reminders.ts`; Test `test/calendar/reminders.test.ts`, `test/calendar/events.test.ts`

**Interfaces:**
- Consumes: `expandSeries` (Task 4), `dateAtTimeToUtc` (Task 2), `getSettings` (Task 1).
- Produces:
```ts
// events.ts
export function eventHash(f: EventFields & { valarmMinutes?: number[] }): string;  // stable JSON → sha-1 hex is fine
export function upsertEvent(sql: SqlStorage, row: Omit<StoredEvent, 'id' | 'hash' | 'updatedAt'>, now: number): { id: number; changed: boolean };
export function deleteSeries(sql: SqlStorage, seriesKey: string): void;      // events + pending reminders
export function getSeries(sql: SqlStorage, seriesKey: string): StoredEvent[];
export function listSeriesKeys(sql: SqlStorage, where?: { sourceId?: number; floatingOrAllDay?: boolean }): string[];
// reminders.ts
export function resolveLeads(ev: StoredEvent, source: SourceRow | null, s: Settings): number[];
export function planReminders(rows: StoredEvent[], occ: Occurrence[], source: SourceRow | null, s: Settings): PlannedReminder[];
export function reconcileSeries(sql: SqlStorage, seriesKey: string, now: number): void;
export function reconcileAll(sql: SqlStorage, now: number, where?: Parameters<typeof listSeriesKeys>[1]): void;
export const WINDOW_MS = 60 * 24 * 3600 * 1000;
```

Rules: `resolveLeads` = `ev.leadMinutes` ?? (origin `ics` && `source.honorValarm` && `valarmMinutes.length` ? `valarmMinutes`) ?? `source.defaultLeadMin` ?? `s.defaultLeadMin`, de-duplicated. All-day → one reminder at `dateAtTimeToUtc(date, s.allDayTime, zone)`, key suffix `allday`, `leadMin: null`. `reconcileSeries` expands over `[now, now + WINDOW_MS)` and diffs against `reminders WHERE series_key = ?`: new key → insert `pending` (`next_attempt_at = fire_at`); existing `pending` with different `fire_at` → update `fire_at`, `next_attempt_at`, reset `attempts`; existing `sent`/`skipped`/`failed` with different `fire_at` → reset to `pending` with the new time; `pending` not planned → delete; finished rows not planned → keep. The functions take `sql` only; callers that write wrap them in `hub.ctx.storage.transactionSync(() => …)` so an event and its reminders change atomically.

- [ ] **Step 1: Write the failing tests**

```ts
it('resolves lead precedence', () => {
  expect(resolveLeads(ev({ leadMinutes: [5] }), src({ defaultLeadMin: 30 }), S)).toEqual([5]);
  expect(resolveLeads(ev({ origin: 'ics', valarmMinutes: [10] }), src({ honorValarm: true }), S)).toEqual([10]);
  expect(resolveLeads(ev({ origin: 'ics', valarmMinutes: [10] }), src({ honorValarm: false, defaultLeadMin: 30 }), S)).toEqual([30]);
  expect(resolveLeads(ev({}), null, S)).toEqual([15]);
});
it('plans one 09:00 reminder for all-day events', () => {
  const p = planReminders([allDay], expandSeries([allDay], ...wide, 'Asia/Shanghai'), null, { ...S, timezone: 'Asia/Shanghai' });
  expect(p).toEqual([expect.objectContaining({ key: expect.stringMatching(/\|allday$/), leadMin: null, fireAt: Date.UTC(2026,11,25,1) })]);
});
it('reconcile: insert, reschedule pending, keep sent history, delete stale pending', async () => {
  await inHub(h => {
    upsertEvent(h.sql, apiEvent({ startLocal: '2026-12-01T10:00:00' }), NOW);
    reconcileSeries(h.sql, 'api:x', NOW);
    expect(rem(h)).toEqual([expect.objectContaining({ status: 'pending', fire_at: Date.UTC(2026,11,1,9,45) })]);
    upsertEvent(h.sql, apiEvent({ startLocal: '2026-12-01T11:00:00' }), NOW);
    reconcileSeries(h.sql, 'api:x', NOW);
    expect(rem(h)[0].fire_at).toBe(Date.UTC(2026,11,1,10,45));
    upsertEvent(h.sql, apiEvent({ leadMinutes: [5] }), NOW);
    reconcileSeries(h.sql, 'api:x', NOW);
    expect(rem(h).map(r => r.lead_min)).toEqual([5]);
  });
});
it('identical upsert reports changed=false and leaves a sent reminder alone', async () => { /* mark sent, upsert same payload, reconcile, status stays 'sent' */ });
it('deleteSeries removes pending reminders but keeps sent ones', async () => { /* ... */ });
```
(`inHub(fn)` = `runInDurableObject(env.HUB.get(env.HUB.idFromName(crypto.randomUUID())), fn)` in `test/helpers.ts` — a fresh Hub per test. `NOW = Date.UTC(2026,10,17)`; `S` = default settings.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: store events and reconcile planned reminders`

---

### Task 6: Message formatting and Bark channel

**Files:** Create `src/notify/message.ts`, `src/notify/bark.ts`; Modify `src/hub.ts` (default `deps.channel` reads the `channels` row `kind='bark'`); Test `test/notify/*.test.ts`

**Interfaces:**
- Produces: `formatReminder(i: { title: string; location: string | null; sourceName: string | null; occStartUtc: number; now: number; tz: string; allDay: boolean; late: boolean; origin: string; eventId: number }): NotifyMessage`; `formatSourceAlert(name: string, error: string, origin: string): NotifyMessage`; `class BarkChannel implements Channel { constructor(cfg: { server: string; deviceKey: string }, fetchFn: typeof fetch) }`; `BarkConfig` type `{ server: string; deviceKey: string }`.

Exact formats: title `⏰ <title>`; body timed `HH:mm · in N min` (`N` rounded, `now` if ≤ 0), all-day `Today · all day`; then ` · <location>` and ` · <sourceName>` when present; late adds the prefix `(late) `. URL `<origin>/calendar?event=<eventId>`. Source alert: title `⚠️ Source sync failing`, body `<name>: <error>` (error truncated to 200 chars), URL `<origin>/sources`. Bark request: `POST <server without trailing slash>/push`, `Content-Type: application/json; charset=utf-8`, body `{ device_key, title, body, url, group: 'reminders', level: 'timeSensitive' }`. Success = HTTP 200 and JSON `code === 200`; anything else → `{ ok: false, error: 'HTTP <status>: <message or text, ≤200 chars>' }`; thrown fetch → `{ ok: false, error: String(e) }`.

- [ ] **Step 1: Write the failing tests** — format: `formatReminder({...base, occStartUtc: NOW + 15*60e3, now: NOW, tz: 'Asia/Shanghai'})` → `{ title: '⏰ Standup', body: '08:15 · in 15 min · Room 3 · Work', url: 'https://x/calendar?event=7' }`; late prefix; all-day body. Bark: a fake `fetchFn` records the request → assert URL `https://api.day.app/push`, method, JSON body above; `{code:200}` → ok; HTTP 400 `{code:400,message:'device key is empty'}` → `error: 'HTTP 400: device key is empty'`; rejected promise → ok false.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: format reminders and send via Bark`

---

### Task 7: Dispatch and the alarm scheduler

**Files:** Create `src/sched/dispatch.ts`, `src/sched/scheduler.ts`; Modify `src/hub.ts` (`alarm`, `rearm`); Test `test/sched/*.test.ts`

**Interfaces:**
- Consumes: `formatReminder`, `formatSourceAlert` (Task 6), `reconcileAll` (Task 5), `pickDueSource`/`syncSource` (Task 8 — in this task, `runAlarm` takes them via the `steps` parameter so Task 8 plugs in without editing the scheduler).
- Produces:
```ts
export const RETRY_DELAYS_MIN = [1, 5, 15];
export function dispatchDue(sql: SqlStorage, deps: HubDeps, origin: string): Promise<number>;   // returns number sent
export function computeNextAlarm(sql: SqlStorage, now: number): number;
export function runMaintenance(sql: SqlStorage, now: number): void;
export function runAlarm(hub: Hub, steps?: { syncOne?: (hub: Hub) => Promise<void> }): Promise<void>;
// Hub
rearm(): Promise<void>;   // setAlarm(max(computeNextAlarm, now + 1000)) only if it differs from getAlarm()
```

Dispatch rules, for each `pending` reminder with `next_attempt_at <= now` (ordered by it, max 20 per run): load the event row by `event_id`; missing → delete reminder, no send. If `now >= occ_start_utc` and `attempts = 0` → `skipped` + delivery log `ok=0, error='event already started'`. If `now >= occ_start_utc` and `attempts > 0` → `failed`. No channel configured → `failed` with error `no notification channel configured`. Otherwise `late = now - fire_at > 120_000`; send; log a `deliveries` row (`kind='reminder'`); success → `sent`, `sent_at`; failure → `attempts += 1`, and if `attempts <= 3` set `next_attempt_at = now + RETRY_DELAYS_MIN[attempts-1]` min else `failed`. Update status right after each send.

`computeNextAlarm` = min of: `MIN(next_attempt_at) WHERE status='pending'`; for each enabled source `COALESCE(last_sync_at + interval_min*60000, now)`; `meta:last_maintenance_at + 24h` (or `now` if never). `runMaintenance`: `reconcileAll`; delete `deliveries` and non-pending `reminders` older than 90 days; delete expired `sessions`; delete `auth_failures` older than 1 h; set `last_maintenance_at`. `runAlarm`: `try { dispatchDue; if (steps.syncOne) await steps.syncOne(hub); if maintenance due → runMaintenance } catch (e) { console.error } finally { await hub.rearm() }`.

- [ ] **Step 1: Write the failing tests** (fake channel records messages; `deps.now` controlled)

```ts
it('sends a due reminder once even if the alarm runs twice', async () => { /* seed event+pending reminder, runAlarm twice → 1 message, status sent */ });
it('never sends a reminder whose event was deleted', async () => { /* delete event row only, runAlarm → 0 messages, reminder gone */ });
it('skips when the event already started', async () => { /* now > occ_start → status skipped, delivery error 'event already started' */ });
it('retries at +1, +5, +15 min then fails', async () => {
  // failing channel; event starts at NOW + 60 min, reminder fires at NOW; advance now to each next_attempt_at and runAlarm
  expect(nextAttempts).toEqual([NOW + 60e3, NOW + 6*60e3, NOW + 21*60e3]);
  expect(finalStatus).toBe('failed');
});
it('marks late reminders', async () => { /* fire_at 10 min ago, event in future → message body starts with '(late) ' */ });
it('arms the alarm at the earliest pending work', async () => {
  // reminder at NOW+10min, source due at NOW+30min, maintenance done now
  await h.rearm(); expect(await state.storage.getAlarm()).toBe(NOW + 10*60e3);
});
it('re-arms even when a step throws', async () => { /* syncOne throws → getAlarm() not null */ });
it('maintenance purges 90-day-old deliveries', async () => { /* ... */ });
```
Also one test that `runDurableObjectAlarm(stub)` invokes `Hub.alarm()` end-to-end.

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement**; `Hub.alarm()` calls `runAlarm(this, { syncOne })` where `syncOne` is wired in Task 8. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: dispatch reminders from a self-arming alarm`

---

### Task 8: Source sync

**Files:** Create `src/sources/sync.ts`; Modify `src/hub.ts` (pass `syncOne`); Test `test/sources/sync.test.ts`

**Interfaces:**
- Consumes: `parseIcs`, `IcsFormatError` (Task 3), `upsertEvent`, `deleteSeries`, `reconcileSeries` (Task 5), `formatSourceAlert` (Task 6).
- Produces: `MAX_ICS_BYTES = 5 * 1024 * 1024`; `pickDueSource(sql, now): SourceRow | null` (enabled, never synced or `last_sync_at + interval_min` ≤ now; oldest first); `syncSource(hub: Hub, source: SourceRow): Promise<void>`; `getSource(sql, id): SourceRow | null`.

Rules: request with `If-None-Match` / `If-Modified-Since` from stored values; 304 → `last_result = 'unchanged'`, success. Reject when `Content-Length` > cap, or when streamed bytes exceed the cap (read the body with a counting reader and abort). Non-2xx, network error, oversize, or `IcsFormatError` → failure: `consecutive_failures += 1`, `last_error`, `last_sync_at = now`, events untouched; when the counter becomes exactly 3, send `formatSourceAlert` through `deps.channel()` and log a `deliveries` row with `kind='source_alert'`. Success → upsert each parsed event as origin `ics`, `seriesKey = ics:<id>:<uid>`, `leadMinutes: null`; delete series of this source not present in the feed; `reconcileSeries` for changed or deleted series only; store new ETag / Last-Modified; `consecutive_failures = 0`; `last_result = 'ok: N events' + (warnings.length ? '; ' + warnings.join('; ') : '')`.

- [ ] **Step 1: Write the failing tests** (fake `deps.fetch`)

```ts
it('imports a feed and creates reminders', async () => { /* weekly feed → events rows, pending reminders, last_result 'ok: 1 events' */ });
it('sends conditional headers and handles 304', async () => { /* second sync: request has If-None-Match "abc"; 304 → 'unchanged' */ });
it('removes events that disappear upstream and their pending reminders', async () => { /* ... */ });
it('reschedules a moved event', async () => { /* DTSTART changes → reminder fire_at changes */ });
it('treats an HTML 200 page as failure and keeps existing events', async () => {
  // first sync ok, second returns '<!doctype html>...' with 200
  expect(eventsCount).toBe(1); expect(source.consecutiveFailures).toBe(1); expect(source.lastError).toMatch(/not a calendar/i);
});
it('rejects feeds over 5 MB', async () => { /* Content-Length 6_000_000 → failure 'too large' */ });
it('alerts exactly once on the third consecutive failure', async () => { /* 4 failures → 1 alert message */ });
it('reports unknown TZID warnings in last_result', async () => { /* unknown-tzid fixture → last_result contains 'China Standard Time' */ });
it('pickDueSource honors interval and enabled', () => { /* ... */ });
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement**; in `Hub`, `syncOne = async h => { const s = pickDueSource(h.sql, h.deps.now()); if (s) await syncSource(h, s); }`. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: sync ICS sources with conditional requests and failure alerts`

---

### Task 9: Auth primitives

**Files:** Create `src/auth/crypto.ts`, `src/auth/cookies.ts`, `src/auth/session.ts`, `src/auth/recovery.ts`; Modify `src/http/app.ts` (middleware); Test `test/auth/*.test.ts`

**Interfaces:**
- Produces:
```ts
// crypto.ts
randomToken(bytes = 32): string;                       // base64url
sha256Hex(s: string): Promise<string>;
signValue(payload: object, secret: string): Promise<string>;   // base64url(json).base64url(hmac-sha256)
verifyValue<T>(token: string, secret: string): Promise<T | null>;   // null on bad signature or payload.exp < Date.now()
timingSafeEqualStr(a: string, b: string): Promise<boolean>;  // compares sha256 digests
// cookies.ts
parseCookies(header: string | null): Record<string, string>;
cookie(name: string, value: string, maxAgeSec: number): string;   // `${name}=${value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=${maxAgeSec}`
// session.ts  (scope: 'full' | 'recovery')
SESSION_COOKIE = '__Host-sid'; SESSION_TTL_MS = 30 * 24 * 3600e3;
createSession(sql, scope, now): Promise<{ token: string; setCookie: string }>;
readSession(sql, cookieHeader, now): Promise<{ idHash: string; scope: 'full' | 'recovery' } | null>;  // extends expiry when last_extended_at older than 1 day
upgradeSession(sql, idHash): void; revokeSession(sql, idHash): void; revokeAllSessions(sql): void;
// recovery.ts
checkRecoveryCode(sql, env: Env, code: string, now: number): Promise<'ok' | 'wrong' | 'locked'>;
```

Middleware in `buildApp`: for non-GET/HEAD requests under `/auth/*` and `/api/admin/*`, reject with 403 `{ error: 'bad origin' }` unless `Origin === env.ORIGIN`. `requireSession('full' | 'any')` helper sets `c.var.session`; missing → 401 `{ error: 'unauthenticated' }`; a `recovery` session on a `full` route → 403 `{ error: 'register a passkey first' }`. Recovery: locked while `meta:recovery_locked_until > now`; a wrong code inserts into `auth_failures`, and when failures in the last hour reach 5, set `recovery_locked_until = now + 1h`; `ok` clears `auth_failures`.

- [ ] **Step 1: Write the failing tests** — sign/verify round-trip, tampered token → null, expired → null; cookie string exact match `'__Host-sid=abc; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=2592000'`; session create → read → revoke → null; sliding extension after 2 days moves `expires_at`; expired session → null; recovery: correct → ok, 5 wrong → 5th returns `wrong`, 6th (even correct) returns `locked`, after `now + 1h` correct → ok; origin middleware: POST `/auth/logout` with `Origin: https://evil.example` → 403.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: add sessions, signed cookies, origin check and recovery rate limit`

---

### Task 10: Passkey and recovery flows

**Files:** Create `src/auth/passkey.ts`, `src/http/auth-routes.ts`; Modify `src/types.ts` (add optional `webauthn` to `HubDeps`), `src/hub.ts` (default `deps.webauthn`); Test `test/auth/passkey.test.ts`

**Interfaces:**
- Consumes: Task 9 helpers; `@simplewebauthn/server` (`generateRegistrationOptions`, `verifyRegistrationResponse`, `generateAuthenticationOptions`, `verifyAuthenticationResponse`).
- Produces: `HubDeps.webauthn?: { verifyRegistration: typeof verifyRegistrationResponse; verifyAuthentication: typeof verifyAuthenticationResponse }` (defaults to the library; tests inject fakes). Routes:

| Route | Auth | Behavior |
|---|---|---|
| `GET /auth/state` | none | `{ setupRequired, authenticated, scope }`; `setupRequired` = no passkey with `rp_id = env.RP_ID` |
| `POST /auth/recovery` `{ code }` | none | `checkRecoveryCode`; ok → `createSession('recovery')`; wrong → 401 `{ error: 'wrong code' }`; locked → 429 `{ error: 'locked' }` |
| `POST /auth/passkey/register/options` | session any | `generateRegistrationOptions({ rpName: 'Reminder Hub', rpID: env.RP_ID, userName: 'owner', userID: <meta:webauthn_user_id bytes, created once>, excludeCredentials: current-RP keys, authenticatorSelection: { residentKey: 'required', userVerification: 'preferred' } })`; set challenge cookie `__Host-chal` = `signValue({ c, p: 'reg', exp: now + 120_000 })`, Max-Age 120 |
| `POST /auth/passkey/register/verify` `{ name, response }` | session any | verify with `expectedChallenge`, `expectedOrigin: env.ORIGIN`, `expectedRPID: env.RP_ID`, `requireUserVerification: false`; store `passkeys` row (id, publicKey, counter, transports JSON, `rp_id`, name ≤ 60 chars, default `'Passkey'`); `upgradeSession`; clear challenge cookie |
| `POST /auth/passkey/login/options` | none | `generateAuthenticationOptions({ rpID: env.RP_ID, userVerification: 'preferred' })`; challenge cookie with `p: 'auth'` |
| `POST /auth/passkey/login/verify` `{ response }` | none | find key by `response.id` with matching `rp_id` (else 401 `{ error: 'unknown passkey' }`); verify with `credential: { id, publicKey, counter, transports }`; update `counter`, `last_used_at`; `createSession('full')` |
| `POST /auth/logout` / `POST /auth/logout-all` | session any | revoke current / all, clear cookie |

A challenge cookie with the wrong purpose, bad signature, or past `exp` → 400 `{ error: 'challenge expired' }`. Registration while authenticated with `full` scope adds another key (multiple keys).

- [ ] **Step 1: Write the failing tests** (fake verifiers return `{ verified: true, registrationInfo: { credential: { id: 'cred1', publicKey: new Uint8Array([1,2]), counter: 0, transports: ['internal'] } } }` and record the arguments)

```ts
it('first-time setup requires the recovery code', async () => {
  expect((await get('/auth/state')).setupRequired).toBe(true);
  expect((await post('/auth/passkey/register/options', {})).status).toBe(401);
  const s = await post('/auth/recovery', { code: 'test-recovery-code' });   // recovery session
  expect((await post('/api/admin/settings', {}, s.cookie)).status).toBe(403);  // full scope required
  await registerPasskey(s.cookie, 'Phone');
  expect((await get('/auth/state', s.cookie))).toMatchObject({ setupRequired: false, scope: 'full' });
});
it('passes RP ID and origin to the verifier', async () => { /* expectedRPID 'localhost', expectedOrigin 'http://localhost:8787' */ });
it('logs in with a registered key and bumps the counter', async () => { /* ... */ });
it('rejects keys registered under another RP ID and reports setupRequired', async () => {
  /* insert passkey with rp_id 'old.example' → state.setupRequired true; login/verify → 401 'unknown passkey' */
});
it('rejects an expired challenge', async () => { /* advance now 121 s between options and verify → 400 */ });
it('supports registering a second key', async () => { /* passkeys count 2 */ });
it('logout-all revokes every session', async () => { /* ... */ });
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: passkey registration, login and recovery-code setup`

---

### Task 11: API tokens and public API v1

**Files:** Create `src/auth/tokens.ts`, `src/http/validate.ts`, `src/http/api-v1.ts`; Test `test/http/api-v1.test.ts`, `test/http/validate.test.ts`

**Interfaces:**
- Consumes: `sha256Hex`, `randomToken` (Task 9); `upsertEvent`, `deleteSeries`, `getSeries`, `reconcileSeries` (Task 5); `expandSeries` (Task 4); `isValidZone`, `utcToWall` (Task 2).
- Produces:
```ts
// tokens.ts — token format 'rh_' + randomToken(32); stored as sha256Hex; prefix = first 10 chars
createToken(sql, name: string, now): Promise<{ id: number; token: string; prefix: string }>;
verifyBearer(sql, authHeader: string | null, now): Promise<number | null>;   // token id; updates last_used_at
listTokens(sql): { id; name; prefix; createdAt; lastUsedAt }[]; revokeToken(sql, id): void;
// validate.ts
type EventPayload = { title: string; start: string; end?: string; all_day?: boolean; timezone?: string;
  location?: string; description?: string; rrule?: string; reminders?: number[] };
validateEventPayload(body: unknown, userTz: string): { ok: true; fields: EventFields } | { ok: false; field: string; error: string };
```

Validation (exact limits): `title` 1–200 chars; `start` ISO 8601 date (all-day) or date-time; with an offset/`Z` → converted to wall clock in `timezone ?? userTz` (and `tzid` = that zone); without offset → wall clock as given with `tzid = timezone ?? null`; `end` ≥ `start`, default `start` (timed) or `start + 1 day` (all-day); `timezone` must pass `isValidZone`; `rrule` must parse with `ICAL.Recur.fromString`; `reminders` ≤ 5 integers each 0–10080; `location`/`description` ≤ 2000 chars.

Routes (all require `Authorization: Bearer <token>`; missing/invalid → 401 `{ error: 'invalid token' }`):
- `PUT /api/v1/events/:external_id` (`external_id` 1–200 chars of `[A-Za-z0-9._:-]`) → 400 `{ error, field }` | 200 `{ id, created: boolean }`; series `api:<external_id>`; reconcile only when `changed`; then `rearm()`.
- `GET /api/v1/events?from=&to=` (ISO; range ≤ 92 days, else 400) → `{ events: [{ external_id | null, origin, title, start, end, all_day, location }] }` across all series (start/end as ISO UTC).
- `DELETE /api/v1/events/:external_id` → 204, or 404 `{ error: 'not found' }`; only `api` origin is reachable by construction.

- [ ] **Step 1: Write the failing tests** — validation table (each limit, offset conversion: `'2026-12-01T10:00:00+08:00'` with `timezone: 'Asia/Shanghai'` → `startLocal '2026-12-01T10:00:00'`, `tzid 'Asia/Shanghai'`; bad rrule → `field: 'rrule'`); routes: no token → 401; PUT creates and returns `created: true`, second identical PUT `created: false` and reminder count unchanged; GET range includes the event; DELETE → 204 then 404; GET range of 100 days → 400; a revoked token → 401.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: bearer-token API for events`

---

### Task 12: Admin API for the web UI

**Files:** Create `src/http/admin.ts`, `src/export/backup.ts`; Test `test/http/admin.test.ts`, `test/export/backup.test.ts`

**Interfaces:**
- Consumes: everything above. All routes `requireSession('full')`; every mutating route ends with `rearm()`.
- Produces (JSON in/out; ids are numbers):

| Route | Notes |
|---|---|
| `GET /api/admin/calendar?from&to` | range ≤ 92 days; expands every series on demand; each item `{ eventId, occKey, origin, sourceId, color, title, location, startUtc, endUtc, allDay, editable, reminders: [{ fireAt, status }] }`; `editable` = origin `manual` |
| `GET/POST /api/admin/sources`, `PATCH/DELETE /api/admin/sources/:id`, `POST /api/admin/sources/:id/sync` | POST validates `url` (https only), `name` 1–60, `color` `#rrggbb`, `intervalMin` ≥ 15; PATCH of lead/VALARM settings → `reconcileAll(sql, now, { sourceId })`; DELETE removes its events and pending reminders; `sync` runs `syncSource` now and returns the updated row |
| `POST /api/admin/events`, `PATCH/DELETE /api/admin/events/:id` | `manual` origin uses `validateEventPayload`; PATCH on non-manual → 409 `{ error: 'read-only' }`; DELETE allowed for `manual` and `api`, 409 for `ics` |
| `GET/PUT /api/admin/settings` | PUT validates `timezone` (`isValidZone`), `defaultLeadMin` 0–10080, `allDayTime` `HH:MM`; timezone or allDayTime change → `reconcileAll(sql, now, { floatingOrAllDay: true })`; defaultLeadMin change → `reconcileAll(sql, now)` |
| `GET/PUT /api/admin/channel`, `POST /api/admin/channel/test` | Bark `{ server (https, default 'https://api.day.app'), deviceKey }`; GET masks the key to its last 4 chars; test sends `{ title: 'Reminder Hub', body: 'Test notification' }` and returns the `SendResult` |
| `GET /api/admin/passkeys`, `DELETE /api/admin/passkeys/:id` | list includes `stale: rp_id !== env.RP_ID`; deleting the last valid key returns 200 with `{ warning: 'no valid passkeys left; recovery code required to log in' }` |
| `GET/POST /api/admin/tokens`, `DELETE /api/admin/tokens/:id` | POST returns the token once |
| `GET /api/admin/log?limit=100` | `{ failedReminders, deliveries, sourceErrors }`: failed reminders first, then deliveries newest first, then sources with `consecutive_failures > 0` |
| `GET /api/admin/export.json`, `GET /api/admin/export.ics` | `Content-Disposition: attachment`; JSON = `{ version: 1, exportedAt, settings, sources, events }` (no secrets, no passkeys, no tokens, Bark key masked); ICS = one VEVENT per event row with DTSTART/TZID, DURATION, RRULE, EXDATE, RECURRENCE-ID, built with `ical.js` |

- [ ] **Step 1: Write the failing tests** — unauthenticated → 401 on each route group; calendar returns colored items in range; source create rejects `http://` URL and `intervalMin: 10`; **changing timezone from `UTC` to `Asia/Shanghai` moves a floating event's pending reminder by −8 h while a `tzid: 'Europe/Berlin'` event's reminder is unchanged**; changing `allDayTime` to `07:30` moves all-day reminders; PATCH an `ics` event → 409; channel GET masks the key; deleting the last passkey returns the warning; export.json contains no `deviceKey` in clear and no `passkeys`; export.ics re-parses with `parseIcs` to the same number of events.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: admin API for calendar, sources, settings and export`

---

### Task 13: Web app shell and auth screens

**Files:** Create `web/package.json`, `web/vite.config.ts`, `web/index.html`, `web/src/{main.tsx,api.ts,strings.ts,router.tsx,auth.tsx}`, `web/src/pages/{Setup.tsx,Login.tsx}`; Modify root `package.json` (`build:web` = `npm --prefix web run build`, `deploy` builds web first); Test `web/src/api.test.ts`

**Interfaces:**
- Consumes: `/auth/*` routes (Task 10).
- Produces: `api<T>(path: string, init?: { method?: string; body?: unknown }): Promise<T>` (JSON, `credentials: 'same-origin'`, throws `ApiError { status, error }`); `useAuth(): { state, refresh }`; routes `/setup`, `/login`, `/calendar`, `/sources`, `/events`, `/settings`, `/log`; nav guards: `setupRequired` → `/setup`, unauthenticated → `/login`, `recovery` scope → `/setup` step 2.

Setup flow: step 1 recovery code → step 2 name + "Register passkey" (`startRegistration({ optionsJSON })` from `@simplewebauthn/browser`) → on success PUT `/api/admin/settings { timezone: Intl.DateTimeFormat().resolvedOptions().timeZone }` → `/calendar`. Login: one "Sign in with passkey" button (`startAuthentication`) and a "Use recovery code" link that goes to Setup step 1. Vite dev server proxies `/api` and `/auth` to `http://localhost:8787`; build output `web/dist`.

- [ ] **Step 1: Write the failing test** — `api()` with a stubbed `fetch`: 401 JSON `{ error: 'unauthenticated' }` throws `ApiError` with `status 401`; JSON body serialized with `Content-Type: application/json`.
- [ ] **Step 2: Run** `npm --prefix web test` — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; `npm run build:web` succeeds and produces `web/dist/index.html`.
- [ ] **Step 5: Commit** — `feat: web shell with passkey setup and login`

---

### Task 14: Web pages — calendar and management

**Files:** Create `web/src/pages/{Calendar.tsx,Sources.tsx,Events.tsx,Settings.tsx,Log.tsx}`, `web/src/calendar/grid.ts`, `web/src/components/{EventForm.tsx,EventDetail.tsx}`; Test `web/src/calendar/grid.test.ts`

**Interfaces:**
- Consumes: `/api/admin/*` (Task 12), `api()` (Task 13).
- Produces: `monthGrid(year: number, month: number /*1-12*/): string[][]` — 6 weeks × 7 `YYYY-MM-DD`, weeks start Monday; `weekDays(date: string): string[]` — Monday-first 7 days; display times with `Intl.DateTimeFormat(undefined, { timeZone: settings.timezone })`.

Page contents follow spec 5.8 exactly. Calendar: month/week toggle, prev/next/today, items colored by source, clicking opens `EventDetail` with reminder statuses and (manual only) edit/delete; API events show delete only; ICS events read-only. Sources: table with last result (warnings visible), failing sources in red, "Sync now". Settings: time zone select (`Intl.supportedValuesOf('timeZone')`), default lead, all-day time, Bark form + "Send test", passkeys list (stale badge, add/delete with the last-key warning), tokens (create shows the token once with a copy button), export links. Log: failed reminders pinned and highlighted, then deliveries, then source errors.

- [ ] **Step 1: Write the failing tests** — `monthGrid(2026, 11)[0][0] === '2026-10-26'`, `monthGrid(2026, 11)[5][6] === '2026-12-06'`, every row has 7 cells; `weekDays('2026-11-19')` → `'2026-11-16'…'2026-11-22'`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; `npm run build:web` succeeds.
- [ ] **Step 5: Commit** — `feat: calendar, sources, events, settings and log pages`

---

### Task 15: End-to-end smoke, CPU check, deploy docs

**Files:** Create `e2e/smoke.spec.ts`, `playwright.config.ts`, `.dev.vars.example`, `README.md`

**Interfaces:**
- Consumes: the whole app via `wrangler dev` (`webServer` in `playwright.config.ts`: `npm run build:web && wrangler dev --port 8787`, `.dev.vars` with `RECOVERY_CODE=e2e-code`, `SESSION_SECRET=e2e-secret`).

- [ ] **Step 1: Write the smoke test** — Chromium; `const cdp = await page.context().newCDPSession(page); await cdp.send('WebAuthn.enable'); await cdp.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true } })`. Flow: open `/` → redirected to `/setup` → enter `e2e-code` → register passkey "E2E" → lands on `/calendar` → create a manual event tomorrow 10:00 → it appears in the week view → log out → `/login` → sign in with passkey → `/calendar` visible.
- [ ] **Step 2: Run** `npx playwright test` — Expected: PASS.
- [ ] **Step 3: CPU check (spec §6)** — deploy to a test Worker on the Free plan, add the largest real ICS you have as a source, click "Sync now", and read the invocation's CPU time with `wrangler tail --format json` (`cpuTime`) or in the dashboard. Record the number in `README.md`. If it exceeds 10 ms consistently, note in the README that the Paid plan is required for that feed size; no code change.
- [ ] **Step 4: Write `README.md`** — prerequisites; `npm i`; set `RP_ID`/`ORIGIN` in `wrangler.jsonc` **once** (custom domain or `<name>.<account>.workers.dev`) and why they must not change; `wrangler secret put RECOVERY_CODE` / `SESSION_SECRET` (`openssl rand -base64 32`); `npm run deploy`; first-time setup; adding a second passkey; using an API token (`curl -X PUT …/api/v1/events/my-id -H 'Authorization: Bearer rh_…' -d '{"title":"…","start":"…"}'`); what to do after a domain change (log in with the recovery code, register new keys, delete stale ones); exporting a backup; the measured CPU number.
- [ ] **Step 5: Final verification** — `npm test && npm run typecheck && npm --prefix web test && npm run build:web && npx playwright test` all pass.
- [ ] **Step 6: Commit** — `docs: deployment guide and end-to-end smoke test`
