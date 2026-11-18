> **Superseded on 2026-11-18** by `2026-11-18-reminder-hub-rust.md` (all-Rust implementation). Kept for history.

# Personal Reminder Hub Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a single-user serverless reminder hub on Cloudflare Workers that merges polled ICS feeds, API-pushed events, and manual entries; dispatches Bark push reminders via a self-arming Durable Object alarm; and provides a passkey-protected management SPA.

**Architecture:** A stateless Worker entry router serves the static SPA via `ASSETS` and forwards `/api/*` and `/auth/*` requests to a singleton SQLite-backed Durable Object `Hub` (`env.HUB.idFromName("main")`). `Hub` owns all persistent state, authentication enforcement, and a single multiplexed alarm targeting the earliest pending reminder, retry, source sync, or daily maintenance task. Pure domain modules (time-zone conversion, ICS parsing, recurrence expansion, reminder planning, message formatting) are unit-tested directly without Durable Object overhead; stateful integration and alarm behavior are tested under `@cloudflare/vitest-pool-workers`.

**Tech Stack:** TypeScript, Wrangler, Durable Objects (SQLite storage API + alarms), Hono (routing inside `Hub`), `ical.js` (ICS parsing and `RRULE` iteration), `@simplewebauthn/server` v13 + `@simplewebauthn/browser`, Vite + Preact (web UI), Vitest 4 + `@cloudflare/vitest-pool-workers` (`cloudflareTest` plugin), Playwright (end-to-end smoke test).

**Spec:** [`docs/2026-11-17-design.md`](../../2026-11-17-design.md) — read before starting any task.

## Global Constraints

- **Safe File Disposal:** Never delete files with `rm`, `rmdir`, `unlink`, `git clean`, `find -delete`, or programmatic unlink; move removed files to Trash (`trash <path>` or `mv <path> ~/.Trash/`).
- **Workers Free Quota Compliance:** Target ≤ 10 ms CPU and ≤ 50 subrequests per invocation. Do not use KV, D1, or Cron Triggers.
- **Singleton Actor:** Use exactly one Durable Object instance: `env.HUB.idFromName("main")`.
- **Time Representations:** Store and pass all timestamps as UTC epoch milliseconds (`number`). Store event wall-clock times as ISO strings: `YYYY-MM-DDTHH:mm:ss` for timed events and `YYYY-MM-DD` for all-day events.
- **Canonical Spec Constants (do not alter):**
  - Rolling expansion window: **60 days**; retention for `deliveries` and finished `reminders`: **90 days**.
  - Global default lead time: **15 min**; default all-day reminder time: **`09:00`** local.
  - ICS sync cadence: default **30 min**, minimum **15 min**; feed size cap: **5 MB** (`5 * 1024 * 1024` bytes); channel alert threshold: **3** consecutive sync failures.
  - Reminder send retry delays: **1, 5, 15 min**, then mark `failed`.
  - Auth & session limits: recovery code rate limit **5** wrong attempts/hour → **1 hour** lockout; WebAuthn challenge cookie TTL **2 min**; sliding session TTL **30 days**.
- **Deployment Configuration:** Secrets via `wrangler secret`: `RECOVERY_CODE`, `SESSION_SECRET`. Vars in `wrangler.jsonc`: `RP_ID`, `ORIGIN`. Pinned once at deployment.
- **Alarm Lifecycle:** Every mutating DO entry point (`fetch` routes that change state) ends by calling `Hub.rearm()`; `alarm()` always invokes `Hub.rearm()` inside a `finally` block.
- **Deterministic Dependency Injection:** Clock (`now()`), outbound `fetch`, notification channel resolution, and WebAuthn verifiers are injected via `Hub.deps` so unit and integration tests never touch the network or wall clock.
- **UI Localization Readiness:** Keep all UI copy in English and centralize strings in `web/src/strings.ts`.
- **Untrusted text:** Feed and event text is always rendered as text — never `dangerouslySetInnerHTML`; the SPA ships a CSP (Spec §11.6).
- **Commit Discipline:** Commit after every completed task using Conventional Commits, with no attribution lines.

## Review Focus

1. **DST transitions:** A weekly `09:00` event in `America/New_York` stays at `09:00` local across March and November DST transitions, and a nonexistent spring-forward wall time shifts **forward** by the gap in zones on both sides of UTC (`America/New_York` `2026-03-08T02:30` → `03:30`; `Europe/Berlin` `2026-03-29T02:30` → `03:30`). Verified in Tasks 2 and 4.
2. **Unknown `TZID` in an ICS feed:** Non-IANA identifiers (e.g., Outlook's `China Standard Time` without a VTIMEZONE mapping to IANA) fall back to the user's configured time zone and emit a visible warning on the source's `last_result` rather than failing silently. Verified in Tasks 3 and 8.
3. **HTTP 200 HTML error pages:** Feeds returning `<!doctype html>...` with HTTP 200 fail structural `VCALENDAR` parsing, count as a sync failure, and leave previously synced events and reminders intact. Verified in Task 8.
4. **Alarm replay, stale reminders, and hot loops:** Running `alarm()` twice does not double-send a `sent` reminder; a reminder whose parent event was deleted prior to dispatch is purged without sending; and a step that throws on every run never re-arms sooner than `now + 60 s` after the first error (Spec §7.2). Verified in Task 7 and Task 8.
5. **Settings updates:** Changing the user's global `timezone` or `allDayTime` queues and then recomputes all pending reminders for floating-time and all-day events (while leaving explicit-`TZID` events untouched unless `defaultLeadMin` changed). Verified in Task 12.

## Architecture Decisions This Plan Implements (Spec §6–§11)

The spec is now a full architecture design; this plan implements it without deviation. The decisions most likely to be missed when reading a single task:

- **No `occurrences` table** (Spec §6.3): series are expanded on demand; `reminders` rows carry `occ_start_utc`. `reminders` is the current schedule; `deliveries` is the append-only history.
- **Inline vs queued reconciliation** (Spec §7.3): single-series edits reconcile inline in the same transaction; settings changes, source-setting changes, syncs, and the daily window roll enqueue series into `reconcile_queue`, drained ≤ 10 per alarm wake.
- **Expansion anchor + budget** (Spec §7.4): resume RRULE iteration from a persisted anchor; 2,000 raw slots per call; statuses `complete | catching_up | too_dense` are persisted and shown.
- **Invariant 2 / hot-loop prevention** (Spec §6.3, §7.1–§7.2): every alarm step isolates its own errors and records progress; a top-level failure applies an exponential error floor (1 → 15 min).
- **ICS matrix** (Spec §9): cancelled single instances become EXDATEs; cancelled masters drop the series; `RDATE` is ignored with a warning; `RANGE=THISANDFUTURE` is a single-instance override; `VALARM` only relative-to-start triggers ≤ 0.
- **All-day reminders**: exactly one at `settings.allDayTime`; lead times do not apply.
- **WebAuthn**: discoverable credentials (`residentKey: 'required'`), `userVerification: 'preferred'` with `requireUserVerification: false` in **both** registration and authentication verification; rename keys; confirmation required before revoking the last valid key.
- **Calendar week start:** Monday-first.

---

## File Structure

```
cf-reminder-calendar/
  package.json  tsconfig.json  wrangler.jsonc  vitest.config.ts
  src/
    index.ts              Worker entry: route /api/* & /auth/* to Hub, else ASSETS; re-export Hub
    env.ts                Env interface
    types.ts              Shared domain types (below)
    hub.ts                Hub DO: sql, deps, migrate, fetch → Hono app, alarm, rearm
    db/schema.ts          MIGRATIONS + migrate(sql)
    db/settings.ts        getSettings / putSettings / getMeta / setMeta
    time/zone.ts          Wall-clock ↔ UTC conversions via Intl.DateTimeFormat
    ics/parse.ts          ICS text → ParsedIcs
    ics/expand.ts         Series (parent + overrides) → sorted Occurrences in [fromUtc, toUtc)
    calendar/events.ts    Event store (upsert/delete/list/hash series)
    calendar/reminders.ts resolveLeads / planReminders (pure) + reconcileSeries
    notify/message.ts     formatReminder / formatSourceAlert
    notify/bark.ts        BarkChannel
    sched/dispatch.ts     dispatchDue
    sched/scheduler.ts    computeNextAlarm, runAlarm, runMaintenance, error backoff
    sched/queue.ts        enqueueSeries, enqueueWhere, processQueue, queueLength
    sources/sync.ts       syncSource, pickDueSource, getSource
    auth/crypto.ts        randomToken, sha256Hex, signValue, verifyValue, timingSafeEqualStr
    auth/cookies.ts       parseCookies, cookie
    auth/session.ts       createSession, readSession, upgradeSession, revokeSession, revokeAllSessions
    auth/recovery.ts      checkRecoveryCode with hourly rate limit + lockout
    auth/passkey.ts       WebAuthn registration/login flows (using injected verifiers)
    auth/tokens.ts        createToken, verifyBearer, listTokens, revokeToken
    http/app.ts           buildApp(hub): Hono app + Origin CSRF & session middleware
    http/auth-routes.ts   /auth/* routes
    http/api-v1.ts        /api/v1/* bearer-authenticated routes
    http/admin.ts         /api/admin/* session-authenticated routes
    http/validate.ts      Request payload validation for events
    export/backup.ts      exportJson / exportIcs
  test/                   Mirrors src/ (*.test.ts) + test/helpers.ts + test/fixtures/*.ics
  web/                    Vite + Preact SPA (index.html, src/…)
  e2e/smoke.spec.ts       Playwright E2E test using CDP virtual WebAuthn authenticator
  README.md               Deployment, configuration, and operations guide
```

### Shared Types (`src/types.ts`, created in Task 1, shared across modules)

```ts
export type Origin = 'ics' | 'api' | 'manual';
export type ReminderStatus = 'pending' | 'sent' | 'failed' | 'skipped';

export interface EventFields {
  title: string; description: string | null; location: string | null;
  allDay: boolean;
  startLocal: string;          // 'YYYY-MM-DDTHH:mm:ss' or 'YYYY-MM-DD'
  tzid: string | null;         // IANA zone, 'UTC', or null = floating (uses settings.timezone)
  durationSec: number;
  rrule: string | null;        // RRULE value without leading 'RRULE:'
  exdates: string[];           // wall-clock strings in the event's zone
  leadMinutes: number[] | null;   // explicit overrides (api/manual); null = fall through
}
export type ExpandStatus = 'complete' | 'catching_up' | 'too_dense';
export interface Anchor { slot: string; index: number; hash: string; }   // Spec §7.4
export interface StoredEvent extends EventFields {
  id: number; origin: Origin; sourceId: number | null; seriesKey: string;
  uid: string | null; recurrenceId: string | null; externalId: string | null;
  valarmMinutes: number[]; hash: string; updatedAt: number;
  expandAnchor: Anchor | null; expandStatus: ExpandStatus; expandWarning: string | null;
}
export interface Occurrence { eventId: number; occKey: string; startUtc: number; endUtc: number; }
export interface ExpandResult {
  occurrences: Occurrence[];
  anchor: Anchor | null;        // new anchor for the parent row (null = non-recurring or unchanged)
  status: ExpandStatus; warning: string | null;
}
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
  channel(): Channel | null;   // null when no notification channel is configured/enabled
  webauthn: {                  // defaults to @simplewebauthn/server (Task 10); tests inject fakes
    verifyRegistration: typeof import('@simplewebauthn/server').verifyRegistrationResponse;
    verifyAuthentication: typeof import('@simplewebauthn/server').verifyAuthenticationResponse;
  };
}
```

Identity key format:
- `seriesKey`: `ics:<sourceId>:<uid>`, `api:<externalId>`, or `manual:<eventId>`
- `occKey`: `<seriesKey>|<slot>` where `slot` is the original occurrence wall-clock start (recurring), the override's `recurrenceId`, or `'single'` (non-recurring)
- `reminderKey`: `<occKey>|<leadMin>` (timed) or `<occKey>|allday` (all-day)

---

### Task 1: Scaffold, Worker routing, and `Hub` with SQLite schema

**Files:**
- Create: `package.json`, `tsconfig.json`, `wrangler.jsonc`, `vitest.config.ts`, `.gitignore`, `src/index.ts`, `src/env.ts`, `src/types.ts`, `src/hub.ts`, `src/db/schema.ts`, `src/db/settings.ts`, `src/http/app.ts`
- Test: `test/hub.test.ts`

**Interfaces:**
- Produces: `class Hub extends DurableObject<Env>` exposing `sql: SqlStorage` (assigned `ctx.storage.sql` in constructor), `deps: HubDeps`, `rearm(): Promise<void>` (no-op stub in this task), `fetch(req)`, and `alarm()` (stub); `migrate(sql: SqlStorage): void`; `getSettings(sql): Settings`; `putSettings(sql, patch: Partial<Settings>): void`; `getMeta(sql, key): string | null`; `setMeta(sql, key, value): void`; `buildApp(hub: Hub): Hono`; `Env { HUB: DurableObjectNamespace<Hub>; ASSETS: Fetcher; RP_ID: string; ORIGIN: string; RECOVERY_CODE: string; SESSION_SECRET: string }`.

- [ ] **Step 1: Scaffold**

`git init`; `npm i hono ical.js @simplewebauthn/server`; `npm i -D wrangler typescript vitest@^4.1.0 @cloudflare/vitest-pool-workers @cloudflare/workers-types`.
`.gitignore`: `node_modules`, `.wrangler`, `web/dist`, `.dev.vars`.

`wrangler.jsonc`: `name: "reminder-hub"`, `main: "src/index.ts"`, `compatibility_date: "2026-11-01"`, `compatibility_flags: ["nodejs_compat"]`, `durable_objects.bindings: [{ name: "HUB", class_name: "Hub" }]`, `migrations: [{ tag: "v1", new_sqlite_classes: ["Hub"] }]`, `vars: { RP_ID: "localhost", ORIGIN: "http://localhost:8787" }`, `assets: { directory: "./web/dist", binding: "ASSETS", not_found_handling: "single-page-application", run_worker_first: ["/api/*", "/auth/*"] }`.

`vitest.config.ts`: `cloudflareTest({ wrangler: { configPath: "./wrangler.jsonc" }, miniflare: { bindings: { RECOVERY_CODE: "test-recovery-code", SESSION_SECRET: "test-session-secret" } } })`, `test.include: ["test/**/*.test.ts"]`. Create `web/dist/.gitkeep` so the assets directory exists before the SPA is built. Scripts in `package.json`: `test` (`vitest run`), `typecheck` (`tsc --noEmit`), `dev` (`wrangler dev`), `deploy` (`wrangler deploy`).

- [ ] **Step 2: Write the failing tests**

```ts
// test/hub.test.ts
import { describe, it, expect } from 'vitest';
import { env } from 'cloudflare:workers';
import { runInDurableObject, createExecutionContext } from 'cloudflare:test';
import worker from '../src/index';
import { migrate } from '../src/db/schema';
import { getSettings } from '../src/db/settings';

const hub = () => env.HUB.get(env.HUB.idFromName('main'));

describe('Hub scaffold', () => {
  it('routes /api/health to the Hub', async () => {
    const res = await worker.fetch(new Request('http://localhost/api/health'), env, createExecutionContext());
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
  });

  it('creates every table on first start and is idempotent', async () => {
    await runInDurableObject(hub(), (h: any) => {
      const names = h.sql.exec("SELECT name FROM sqlite_master WHERE type='table'").toArray().map((r: any) => r.name);
      for (const t of ['settings','sources','events','reminders','reconcile_queue','deliveries','channels','passkeys','sessions','api_tokens','auth_failures'])
        expect(names).toContain(t);
      expect(() => migrate(h.sql)).not.toThrow();
    });
  });

  it('returns default settings', async () => {
    await runInDurableObject(hub(), (h: any) => {
      expect(getSettings(h.sql)).toEqual({ timezone: 'UTC', defaultLeadMin: 15, allDayTime: '09:00' });
    });
  });
});
```

- [ ] **Step 3: Run test to verify it fails**

Run: `npx vitest run test/hub.test.ts` — Expected: FAIL (modules missing).

- [ ] **Step 4: Implement**

`src/index.ts`: route paths starting with `/api/` or `/auth/` to `env.HUB.get(env.HUB.idFromName('main')).fetch(req)`; route all other paths to `env.ASSETS.fetch(req)`. Re-export `export { Hub } from './hub'`.

`src/hub.ts`: store `this.sql = ctx.storage.sql`; in constructor run `ctx.blockConcurrencyWhile(async () => migrate(this.sql))`; initialize `deps` to `{ now: () => Date.now(), fetch: (...a) => fetch(...a), channel: () => null, webauthn: <throwing placeholders> }` (Task 6 wires the real Bark channel loader, Task 10 the real WebAuthn verifiers); forward `fetch(req)` to `buildApp(this).fetch(req)`. Add `GET /api/health` returning `{ ok: true }` in `src/http/app.ts`.

`src/db/schema.ts`: define `MIGRATIONS: string[]` applied sequentially inside `sql`, tracked via `_migrations(version INTEGER NOT NULL)`. Migration 1 is **exactly the DDL in Spec §6.2** (copy it verbatim; it includes `events.expand_anchor/expand_status/expand_warning`, `events_source`, and `reconcile_queue`).

`src/db/settings.ts`: store settings values as JSON in `settings`, defaulting to `{ timezone: 'UTC', defaultLeadMin: 15, allDayTime: '09:00' }`. Store internal metadata in the same table under a `meta:` key prefix; the full key list is in Spec §6.2 (`last_maintenance_at`, `last_maintenance_error`, `alarm_error_streak`, `last_alarm_error`, `recovery_locked_until`, `webauthn_user_id`).

- [ ] **Step 5: Run tests** — `npm test` → PASS; `npm run typecheck` → no errors.

- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat: scaffold worker, hub durable object and schema"`

---

### Task 2: Time zone utilities

**Files:** Create `src/time/zone.ts`; Test `test/time/zone.test.ts`

**Interfaces:**
- Produces: `isValidZone(tz: string): boolean`; `wallToUtc(wall: string, tz: string): number` (where `wall` is `YYYY-MM-DDTHH:mm:ss`; nonexistent spring-forward wall times shift forward by the transition gap; ambiguous fall-back wall times resolve to the earlier UTC instant); `utcToWall(utc: number, tz: string): string`; `dateAtTimeToUtc(date: 'YYYY-MM-DD', hhmm: string, tz: string): number`; `addDaysToDate(date: string, days: number): string`.

- [ ] **Step 1: Write the failing tests**

```ts
it('converts wall time in a fixed-offset zone', () => {
  expect(wallToUtc('2026-11-17T09:00:00', 'Asia/Shanghai')).toBe(Date.UTC(2026, 10, 17, 1, 0, 0));
});
it('keeps 09:00 local across New York DST', () => {
  expect(wallToUtc('2026-03-06T09:00:00', 'America/New_York')).toBe(Date.UTC(2026, 2, 6, 14));
  expect(wallToUtc('2026-03-09T09:00:00', 'America/New_York')).toBe(Date.UTC(2026, 2, 9, 13));
});
it('shifts a nonexistent spring-forward time forward (west of UTC)', () => {
  expect(utcToWall(wallToUtc('2026-03-08T02:30:00', 'America/New_York'), 'America/New_York')).toBe('2026-03-08T03:30:00');
});
it('shifts a nonexistent spring-forward time forward (east of UTC)', () => {
  expect(wallToUtc('2026-03-29T02:30:00', 'Europe/Berlin')).toBe(Date.UTC(2026, 2, 29, 1, 30));
  expect(utcToWall(wallToUtc('2026-03-29T02:30:00', 'Europe/Berlin'), 'Europe/Berlin')).toBe('2026-03-29T03:30:00');
});
it('takes the earlier instant for an ambiguous fall-back time east of UTC', () => {
  expect(wallToUtc('2026-10-25T02:30:00', 'Europe/Berlin')).toBe(Date.UTC(2026, 9, 25, 0, 30));
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
- [ ] **Step 3: Implement** using `Intl.DateTimeFormat(undefined, { timeZone, hourCycle: 'h23', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' })` cached per zone in a `Map`. Algorithm (Spec §8, exactly): `g = Date.UTC(wall fields)`; `pre = offset(g − 86_400_000)`, `post = offset(g + 86_400_000)`; candidates `[g − pre, g − post]` sorted ascending and de-duplicated; return the first candidate with `utcToWall(candidate, tz) === wall`; if none round-trips (gap), return `g − pre`. Never use the offset sampled at `g` itself — east of UTC it can already be post-transition and would shift the wall time backward.
- [ ] **Step 4: Run tests** — PASS.
- [ ] **Step 5: Commit** — `feat: add time zone conversion utilities`

---

### Task 3: ICS parsing

**Files:** Create `src/ics/parse.ts`; Test `test/ics/parse.test.ts`, fixtures `test/fixtures/{weekly-ny.ics,override.ics,allday-valarm.ics,unknown-tzid.ics,cancelled.ics,cancelled-instance.ics,orphan-override.ics,rdate.ics}`

**Interfaces:**
- Consumes: `isValidZone` (Task 2).
- Produces:
```ts
export interface ParsedIcsEvent extends Omit<EventFields, 'leadMinutes'> {
  uid: string; recurrenceId: string | null; valarmMinutes: number[];
}
export interface ParsedIcs { events: ParsedIcsEvent[]; warnings: string[]; }
export class IcsFormatError extends Error {}
export function parseIcs(text: string): ParsedIcs;   // throws IcsFormatError if not a valid VCALENDAR
```

Normalization rules:
- Extract wall-clock strings (`YYYY-MM-DDTHH:mm:ss` or `YYYY-MM-DD`) directly from `ICAL.Time` component fields (`year…second`)—never call `toJSDate()`.
- `DTSTART` ending in `Z` sets `tzid: 'UTC'`. Any `TZID` rejected by `isValidZone` falls back to `tzid: null` and pushes warning ``Unknown TZID "<id>" on "<summary>"; using your time zone``.
- `VALUE=DATE` sets `allDay: true`, `startLocal: 'YYYY-MM-DD'`, and whole-day `durationSec` (default `86400`). Timed events compute `durationSec` from `DTEND` or `DURATION` (default `0`).
- Cancellation (Spec §9): a master (`RECURRENCE-ID` absent) with `STATUS:CANCELLED` drops the whole series including its overrides; an override with `STATUS:CANCELLED` is **not** emitted — its `recurrenceId` is appended to the parent's `exdates` so the slot stays suppressed; a cancelled override whose master is absent is ignored. Non-cancelled overrides whose master is absent are emitted unchanged (they expand as stand-alone occurrences).
- `RDATE` present → ignored, warning ``RDATE ignored on "<summary>"``.
- Normalize `RECURRENCE-ID` to a wall-clock string in `recurrenceId`.
- Flatten all `EXDATE` properties (both multi-valued properties and repeated `EXDATE` lines) into `exdates` wall-clock strings.
- Parse `VALARM` subcomponents: honor only relative `TRIGGER` durations ≤ 0 with `RELATED=START` (or default `RELATED`), converting to positive integer minutes before start (`-PT15M` → `15`, `-P1D` → `1440`). Ignore absolute or `RELATED=END` triggers.

- [ ] **Step 1: Write the failing tests** (one per fixture plus invalid input)

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
it('drops a cancelled master with its overrides', () => { expect(parseIcs(fixture('cancelled.ics')).events).toEqual([]); });
it('turns a cancelled single instance into an EXDATE on the parent', () => {
  const { events } = parseIcs(fixture('cancelled-instance.ics'));   // weekly c1, instance 2026-03-09T09:00 cancelled
  expect(events).toHaveLength(1);
  expect(events[0]).toMatchObject({ uid: 'c1', recurrenceId: null });
  expect(events[0].exdates).toContain('2026-03-09T09:00:00');
});
it('keeps an override whose master is absent', () => {
  expect(parseIcs(fixture('orphan-override.ics')).events).toEqual([expect.objectContaining({ recurrenceId: '2026-03-09T09:00:00' })]);
});
it('warns that RDATE is ignored', () => {
  expect(parseIcs(fixture('rdate.ics')).warnings).toEqual([expect.stringContaining('RDATE ignored')]);
});
it('rejects non-calendar input', () => {
  expect(() => parseIcs('<!doctype html><html>error</html>')).toThrow(IcsFormatError);
});
```
(`fixture(name)` loads files via `import.meta.glob('../fixtures/*.ics', { query: '?raw', eager: true })`.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement** with `ICAL.parse` + `new ICAL.Component(jcal)`; wrap any parse error in `IcsFormatError` and verify `comp.name === 'vcalendar'`. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: parse ICS feeds into normalized events`

---

### Task 4: Recurrence expansion

**Files:** Create `src/ics/expand.ts`; Test `test/ics/expand.test.ts`

**Interfaces:**
- Consumes: `wallToUtc`, `dateAtTimeToUtc`, `addDaysToDate` (Task 2); `StoredEvent`, `Occurrence`.
- Produces:
```ts
export const EXPAND_BUDGET = 2_000;          // raw RRULE slots per call (Spec §7.4)
export const ANCHOR_LAG_MS = 86_400_000;     // anchor slot must start before now − 1 day
export function expandSeries(rows: StoredEvent[], fromUtc: number, toUtc: number, userTz: string,
  opts?: { anchorBeforeUtc?: number; budget?: number }): ExpandResult;   // budget defaults to EXPAND_BUDGET; tests pass Infinity
```
`rows` = all stored rows of one `seriesKey` (parent + overrides). `occurrences` are those with `startUtc` in `[fromUtc, toUtc)`, sorted ascending. `opts.anchorBeforeUtc` (reconciliation passes `now − ANCHOR_LAG_MS`) asks for a new anchor: the last raw slot whose start is `< anchorBeforeUtc`; omit it (calendar queries) to leave the anchor untouched (`anchor: null`).

Algorithm (Spec §7.4):
- Effective zone per row: `row.tzid ?? userTz`. Suppressed slots = parent `exdates` ∪ all override `recurrenceId`s.
- Parent with `rrule === null`: one occurrence, slot `'single'` (timed: `wallToUtc`; all-day: `dateAtTimeToUtc(date, '00:00', zone)`), unless suppressed.
- Parent with `rrule`:
  - **Start point:** if `parent.expandAnchor` exists, `anchor.hash === parent.hash`, `wallToUtc(anchor.slot) <= fromUtc`, and the rule has no `BYSETPOS` → rebase: `dtstart = anchor.slot`, and if the rule has `COUNT`, `COUNT = COUNT − anchor.index`; raw index starts at `anchor.index`. Otherwise start at `startLocal`, index 0.
  - Iterate `ICAL.Recur.fromString(rule).iterator(ICAL.Time.fromDateTimeString/fromDateString(dtstart))` over floating wall-clock slots, incrementing the raw index for every slot (suppressed ones included — they count toward `COUNT`). Convert each to `startUtc`; track the last slot with `startUtc < anchorBeforeUtc` as the anchor candidate; skip suppressed slots and slots before `fromUtc`; stop at `toUtc` or rule end → `status: 'complete'`.
  - **Budget:** after `EXPAND_BUDGET` raw slots without finishing → if the last slot is still before `fromUtc`, `status: 'catching_up'` (no occurrences, anchor = furthest slot reached, so the next call resumes); otherwise `status: 'too_dense'`, keep the occurrences found, `warning: 'Recurrence too dense; only the first N occurrences in the window are scheduled'`.
- Each override row: one occurrence, `occKey = ${seriesKey}|${recurrenceId}`, emitted independently (works with or without a parent row).
- `endUtc = startUtc + durationSec * 1000` for timed events, or midnight in `zone` of `addDaysToDate(startLocal, Math.max(1, Math.round(durationSec / 86400)))` for all-day events.

- [ ] **Step 1: Write the failing tests**

```ts
const range = [Date.UTC(2026, 2, 1), Date.UTC(2026, 3, 1)] as const;
const occOf = (...a: Parameters<typeof expandSeries>) => expandSeries(...a).occurrences;
it('expands weekly across DST keeping 09:00 local and honoring EXDATE', () => {
  const occ = occOf([weeklyNy], ...range, 'UTC');  // weeklyNy: start 2026-03-02T09:00, COUNT=4, exdate 03-16
  expect(occ.map(o => o.startUtc)).toEqual([Date.UTC(2026,2,2,14), Date.UTC(2026,2,9,13), Date.UTC(2026,2,23,13)]);
  expect(occ[1].occKey).toBe('ics:1:w1|2026-03-09T09:00:00');
});
it('replaces an overridden slot with the override row, same occKey', () => {
  const occ = occOf([parent, override], ...range, 'UTC');  // override moves 03-09 09:00 → 11:00
  const o = occ.find(o => o.occKey === 'ics:1:o1|2026-03-09T09:00:00')!;
  expect(o.startUtc).toBe(Date.UTC(2026,2,9,15));
  expect(occ.filter(o => o.occKey.endsWith('2026-03-09T09:00:00'))).toHaveLength(1);
});
it('expands an override without a parent row', () => {
  expect(occOf([override], ...range, 'UTC')).toHaveLength(1);
});
it('interprets floating times in the user zone', () => {
  expect(occOf([{ ...single, tzid: null }], ...range, 'Asia/Shanghai')[0].startUtc).toBe(Date.UTC(2026,2,10,1));
});
it('uses slot "single" for non-recurring events and respects the range', () => {
  expect(occOf([single], ...range, 'UTC')[0].occKey).toBe(`${single.seriesKey}|single`);
  expect(occOf([single], Date.UTC(2027,0,1), Date.UTC(2027,1,1), 'UTC')).toEqual([]);
});
it('anchored expansion equals full expansion (differential)', () => {
  const rules = ['FREQ=DAILY', 'FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TH', 'FREQ=MONTHLY;BYMONTHDAY=31',
    'FREQ=YEARLY', 'FREQ=DAILY;COUNT=4000', 'FREQ=WEEKLY;UNTIL=20270101T000000Z'];
  for (const rrule of rules) {
    const ev = makeEvent({ seriesKey: 'manual:9', startLocal: '2016-01-31T08:00:00', rrule, exdates: ['2026-12-03T08:00:00'] });
    const win = [Date.UTC(2026,11,1), Date.UTC(2027,0,30)] as const;
    const full = expandSeries([ev], ...win, 'UTC', { budget: Infinity });            // from DTSTART, no anchor
    let step = expandSeries([ev], ...win, 'UTC', { anchorBeforeUtc: win[0] - ANCHOR_LAG_MS });
    while (step.status === 'catching_up')                                                // resumable catch-up
      step = expandSeries([{ ...ev, expandAnchor: step.anchor }], ...win, 'UTC', { anchorBeforeUtc: win[0] - ANCHOR_LAG_MS });
    const anchored = expandSeries([{ ...ev, expandAnchor: step.anchor }], ...win, 'UTC');
    expect(anchored.occurrences).toEqual(full.occurrences);
  }
});
it('reports catching_up for an ancient daily series and resumes from the returned anchor', () => {
  const ev = makeEvent({ seriesKey: 'manual:8', startLocal: '2010-01-01T08:00:00', rrule: 'FREQ=DAILY' });
  const first = expandSeries([ev], Date.UTC(2026,10,17), Date.UTC(2027,0,16), 'UTC', { anchorBeforeUtc: Date.UTC(2026,10,16) });
  expect(first.status).toBe('catching_up'); expect(first.anchor!.index).toBe(EXPAND_BUDGET - 1);
});
it('ignores an anchor whose hash no longer matches', () => { /* anchor.hash 'old' → iteration restarts at startLocal */ });
it('reports too_dense for a minutely rule', () => {
  const r = expandSeries([makeEvent({ seriesKey: 'manual:7', startLocal: '2026-11-17T00:00:00', rrule: 'FREQ=MINUTELY' })],
    Date.UTC(2026,10,17), Date.UTC(2026,10,20), 'UTC');
  expect(r.status).toBe('too_dense'); expect(r.occurrences).toHaveLength(EXPAND_BUDGET); expect(r.warning).toMatch(/too dense/);
});
```
(Create a `makeEvent(partial)` factory in `test/helpers.ts` for reuse across tasks; defaults: `tzid: 'UTC'`, `durationSec: 3600`, `rrule: null`, `exdates: []`, `valarmMinutes: []`, `leadMinutes: null`, `expandAnchor: null`, `expandStatus: 'complete'`, `expandWarning: null`, `hash` computed from the fields. `single` = `makeEvent({ seriesKey: 'manual:1', startLocal: '2026-03-10T09:00:00' })`.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: expand recurring series into occurrences`

---

### Task 5: Event store, reminder reconciliation, and the reconcile queue

**Files:** Create `src/calendar/events.ts`, `src/calendar/reminders.ts`, `src/sched/queue.ts`; Test `test/calendar/reminders.test.ts`, `test/calendar/events.test.ts`, `test/sched/queue.test.ts`

**Interfaces:**
- Consumes: `expandSeries`, `ANCHOR_LAG_MS` (Task 4), `dateAtTimeToUtc` (Task 2), `getSettings` (Task 1).
- Produces:
```ts
// events.ts
export function eventHash(f: EventFields & { valarmMinutes?: number[] }): string;  // deterministic JSON → sha-256 hex
export function upsertEvent(sql: SqlStorage, row: Omit<StoredEvent, 'id' | 'hash' | 'updatedAt' | 'expandAnchor' | 'expandStatus' | 'expandWarning'>, now: number): { id: number; changed: boolean };
  // a changed row resets expand_anchor = NULL, expand_status = 'complete', expand_warning = NULL
export function deleteSeries(sql: SqlStorage, seriesKey: string, now: number): void;   // events + pending reminders (withdrawal rule below) + queue item
export function getSeries(sql: SqlStorage, seriesKey: string): StoredEvent[];
export type SeriesFilter = { sourceId?: number; floatingOrAllDay?: boolean; recurringOnly?: boolean };
export function listSeriesKeys(sql: SqlStorage, where?: SeriesFilter): string[];
// reminders.ts
export const WINDOW_MS = 60 * 24 * 3600 * 1000;
export function resolveLeads(ev: StoredEvent, source: SourceRow | null, s: Settings): number[];
export function planReminders(rows: StoredEvent[], occ: Occurrence[], source: SourceRow | null, s: Settings): PlannedReminder[];
export function reconcileSeries(sql: SqlStorage, seriesKey: string, now: number): { status: ExpandStatus };
// queue.ts  (Spec §7.3)
export const QUEUE_BATCH = 10;
export function enqueueSeries(sql: SqlStorage, keys: string[], reason: string, now: number): void;   // upsert; keeps the earliest not_before
export function enqueueWhere(sql: SqlStorage, where: SeriesFilter, reason: string, now: number): number;   // returns count
export function processQueue(sql: SqlStorage, now: number, limit = QUEUE_BATCH): { done: number; failed: number };
export function queueLength(sql: SqlStorage): number;
```

Rules:
- `resolveLeads`: `ev.leadMinutes` ?? (`ev.origin === 'ics' && source?.honorValarm && ev.valarmMinutes.length > 0` ? `ev.valarmMinutes` : undefined) ?? `source?.defaultLeadMin` ?? `s.defaultLeadMin`, deduplicated.
- All-day events ignore lead minutes and plan one reminder at `dateAtTimeToUtc(date, s.allDayTime, zone)` with key `<occKey>|allday` and `leadMin: null`.
- `reconcileSeries` calls `expandSeries(rows, now, now + WINDOW_MS, s.timezone, { anchorBeforeUtc: now - ANCHOR_LAG_MS })`, persists the returned `anchor` / `status` / `warning` on the parent row, then diffs planned reminders against `reminders WHERE series_key = ?` exactly per the table in Spec §7.3:
  - Planned, not stored → insert `pending` (`next_attempt_at = fire_at`).
  - Stored `pending`, `fire_at` changed → update `fire_at`, `next_attempt_at`; reset `attempts = 0`, `late = 0`, `last_error = null`.
  - Stored `sent` / `skipped` / `failed`, `fire_at` changed → rewrite as `pending` with the new time, `attempts = 0`, `late = 0`, `last_error = null`, `sent_at = null` (the earlier outcome remains in `deliveries`).
  - Stored `pending`, no longer planned → delete; if `attempts > 0` first insert a `deliveries` row (`kind = 'reminder'`, `ok = 0`, `error = 'withdrawn: event changed or removed'`).
  - Stored finished row, no longer planned → keep.
  - With status `catching_up`, plan nothing yet (do not delete existing pending rows) and return the status so the queue re-enqueues it.
- `processQueue`: take up to `limit` rows with `not_before <= now` ordered by `not_before`; for each, inside its own `try`: `reconcileSeries`; `catching_up` → keep the row with `not_before = now` (continues next wake); otherwise delete the row. On exception: `attempts += 1`, `last_error`, `not_before = now + 2^attempts * 60_000`; when `attempts` reaches 3, delete the row and write `expand_warning = 'Reminder recomputation failed: <error>'` on the parent row. Never throws.
- Keep these modules synchronous over `sql`; callers wrap multi-step writes in `hub.ctx.storage.transactionSync(...)`.

- [ ] **Step 1: Write the failing tests**

```ts
it('resolves lead precedence', () => {
  expect(resolveLeads(ev({ leadMinutes: [5] }), src({ defaultLeadMin: 30 }), S)).toEqual([5]);
  expect(resolveLeads(ev({ origin: 'ics', valarmMinutes: [10] }), src({ honorValarm: true }), S)).toEqual([10]);
  expect(resolveLeads(ev({ origin: 'ics', valarmMinutes: [10] }), src({ honorValarm: false, defaultLeadMin: 30 }), S)).toEqual([30]);
  expect(resolveLeads(ev({}), null, S)).toEqual([15]);
});
it('plans one 09:00 reminder for all-day events', () => {
  const occ = expandSeries([allDay], ...wide, 'Asia/Shanghai').occurrences;
  const p = planReminders([allDay], occ, null, { ...S, timezone: 'Asia/Shanghai' });
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
it('a sent reminder of a rescheduled event becomes pending again; the old send stays in deliveries', async () => { /* ... */ });
it('withdrawing a retrying reminder logs a withdrawn delivery', async () => { /* pending attempts=1, deleteSeries → deliveries error 'withdrawn: event changed or removed' */ });
it('deleteSeries removes pending reminders but keeps sent ones', async () => { /* ... */ });
it('persists the anchor and expand status on the parent row', async () => { /* ancient daily series → expand_status 'catching_up', expand_anchor set, no reminders yet */ });
it('processQueue drains at most QUEUE_BATCH series and keeps catching_up items', async () => { /* enqueue 12 → done 10, length 2; catching_up item stays */ });
it('processQueue backs off a throwing series and drops it after 3 attempts with a warning', async () => {
  // corrupt one row (rrule 'FREQ=NOPE') so reconcileSeries throws
  // runs at now, now+2m, now+2m+4m → row gone, parent.expand_warning starts with 'Reminder recomputation failed'
});
```
(`inHub(fn)` = `runInDurableObject(env.HUB.get(env.HUB.idFromName(crypto.randomUUID())), fn)` in `test/helpers.ts` — isolated `Hub` per test. `NOW = Date.UTC(2026, 10, 17)`; `S` = default settings.)

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: store events, reconcile reminders, and queue bulk recomputation`

---

### Task 6: Message formatting and Bark channel

**Files:** Create `src/notify/message.ts`, `src/notify/bark.ts`; Modify `src/hub.ts` (default `deps.channel` reads enabled `channels` row `kind='bark'`); Test `test/notify/*.test.ts`

**Interfaces:**
- Produces: `formatReminder(i: { title: string; location: string | null; sourceName: string | null; occStartUtc: number; now: number; tz: string; allDay: boolean; late: boolean; origin: string; eventId: number }): NotifyMessage`; `formatSourceAlert(name: string, error: string, origin: string): NotifyMessage`; `class BarkChannel implements Channel { constructor(cfg: BarkConfig, fetchFn: typeof fetch) }`; `BarkConfig` type `{ server: string; deviceKey: string }`.

Formatting and transport rules:
- `formatReminder`:
  - `title`: `⏰ ${title}`
  - `body` base: timed events `HH:mm · in N min` (`Math.round((occStartUtc - now) / 60_000)`, or `HH:mm · now` when ≤ 0); all-day events `Today · all day`.
  - Append ` · ${location}` if present, then ` · ${sourceName}` if present.
  - Prepend `(late) ` to `body` when `late === true`.
  - `url`: `${origin}/calendar?event=${eventId}`.
- `formatSourceAlert`:
  - `title`: `⚠️ Source sync failing`, `body`: `${name}: ${error.slice(0, 200)}`, `url`: `${origin}/sources`.
- `BarkChannel`:
  - Sends `POST ${server.replace(/\/+$/, '')}/push` with `Content-Type: application/json; charset=utf-8` and JSON body `{ device_key, title, body, url, group: 'reminders', level: 'timeSensitive' }`.
  - Succeeds only when HTTP status is 200 and response JSON has `code === 200`. Otherwise returns `{ ok: false, error: 'HTTP <status>: <message or body text, ≤200 chars>' }`; network exceptions return `{ ok: false, error: String(e) }`.

- [ ] **Step 1: Write the failing tests** — formatting: `formatReminder({...base, occStartUtc: NOW + 15*60e3, now: NOW, tz: 'Asia/Shanghai'})` → `{ title: '⏰ Standup', body: '08:15 · in 15 min · Room 3 · Work', url: 'https://x/calendar?event=7' }`; late prefix; all-day body. Bark: fake `fetchFn` records request → assert URL `https://api.day.app/push`, method `POST`, JSON payload; `{code:200}` → `{ ok: true }`; HTTP 400 `{code:400,message:'device key is empty'}` → `{ ok: false, error: 'HTTP 400: device key is empty' }`; rejected `fetchFn` → `{ ok: false }`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: format reminders and send via Bark`

---

### Task 7: Dispatch and the alarm scheduler

**Files:** Create `src/sched/dispatch.ts`, `src/sched/scheduler.ts`; Modify `src/hub.ts` (`alarm`, `rearm`); Test `test/sched/dispatch.test.ts`, `test/sched/scheduler.test.ts`

**Interfaces:**
- Consumes: `formatReminder` (Task 6); `processQueue`, `enqueueWhere` (Task 5); `getMeta` / `setMeta` (Task 1); `syncOne` (Task 8 — injected via `steps` so Task 8 attaches without editing the scheduler).
- Produces:
```ts
export const RETRY_DELAYS_MIN = [1, 5, 15];              // 1 initial attempt + 3 retries (Spec §7.6)
export const DISPATCH_BATCH = 20;
export const LATE_THRESHOLD_MS = 120_000;
export const ERROR_FLOOR_MAX_MIN = 15;
export function dispatchDue(sql: SqlStorage, deps: HubDeps, origin: string): Promise<{ sent: number; failed: number }>;   // never throws
export function computeNextAlarm(sql: SqlStorage, now: number): number;
export function runMaintenance(sql: SqlStorage, now: number): void;   // throws on failure; caller records it
export type AlarmSteps = { syncOne?: (hub: Hub) => Promise<void> };
export function runAlarm(hub: Hub, steps?: AlarmSteps): Promise<void>;
// Hub
rearm(opts?: { errorFloor?: boolean }): Promise<void>;
```

Dispatch rules (`dispatchDue`, Spec §7.6) — up to `DISPATCH_BATCH` `pending` rows with `next_attempt_at <= now`, ordered by `next_attempt_at`; each candidate is processed inside its own `try`:
1. Load the `events` row by `event_id` (plus `sources.name`). Missing → delete the reminder, no send.
2. `now >= occ_start_utc` → `skipped` (delivery `ok = 0`, `error = 'event already started'`) if `attempts === 0`, else `failed`.
3. `deps.channel()` is `null` → `failed`, `last_error = 'no notification channel configured'`, failed delivery logged.
4. Otherwise `late = now - fire_at > LATE_THRESHOLD_MS`; `send`; append a `deliveries` row (`kind = 'reminder'`); persist immediately — success → `sent`, `sent_at = now`, `late`; failure → `attempts += 1`, then `attempts <= 3` ? stay `pending` with `next_attempt_at = now + RETRY_DELAYS_MIN[attempts - 1] * 60_000` : `failed`.
5. **Any exception thrown while handling the candidate** (formatting, channel, SQL) is handled exactly like a failed send in step 4 (`last_error = String(e)`), so a poisonous reminder ends in `failed` after 4 attempts instead of looping.

Scheduler rules:
- `computeNextAlarm` (Spec §7.2) = minimum of: `MIN(next_attempt_at)` over `pending` reminders; `MIN(not_before)` over `reconcile_queue`; per enabled source `COALESCE(last_sync_at + interval_min * 60_000, now)`; `COALESCE(meta:last_maintenance_at + 86_400_000, now)`.
- `runMaintenance` (Spec §7.1): `enqueueWhere(sql, { recurringOnly: true }, 'roll', now)`; delete `deliveries` and non-`pending` reminders with timestamps older than 90 days; delete expired `sessions`; delete `auth_failures` older than 1 hour; set `meta:last_maintenance_at = now`, clear `meta:last_maintenance_error`.
- `runAlarm` (Spec §7.1) — each step isolated:
```ts
let topLevelError = false;
try {
  await dispatchDue(...);                                  // never throws
  processQueue(sql, now);                                  // never throws
  try { await steps?.syncOne?.(hub); } catch (e) { console.error('sync', e); }   // syncSource records its own failure (Task 8)
  if (maintenanceDue) {
    try { runMaintenance(sql, now); }
    catch (e) { setMeta(sql, 'last_maintenance_error', String(e)); setMeta(sql, 'last_maintenance_at', String(now - 23 * 3_600_000)); }
  }
} catch (e) { topLevelError = true; setMeta(sql, 'last_alarm_error', String(e)); console.error('alarm', e); }
finally { await hub.rearm({ errorFloor: topLevelError }); }
```
- `rearm({ errorFloor })`: `streak = errorFloor ? streak + 1 : 0` (persist `meta:alarm_error_streak` only when it changes); `floor = errorFloor ? now + min(2^(streak − 1), ERROR_FLOOR_MAX_MIN) * 60_000 : now + 1000   // 1, 2, 4, 8, 15 min`; `target = max(computeNextAlarm(sql, now), floor)`; call `setAlarm(target)` only if `target !== await ctx.storage.getAlarm()`. HTTP routes call `rearm()` without options (they never raise the floor and never reset a non-zero streak — only an alarm run resets it).

- [ ] **Step 1: Write the failing tests** (fake channel records messages; `deps.now` controlled)

```ts
it('sends a due reminder once even if the alarm runs twice', async () => { /* seed event+pending reminder, runAlarm twice → 1 message, status sent */ });
it('never sends a reminder whose event was deleted', async () => { /* delete event row only, runAlarm → 0 messages, reminder gone */ });
it('skips when the event already started', async () => { /* now > occ_start → status skipped, delivery error 'event already started' */ });
it('retries at +1, +5, +15 min then fails (4 attempts total)', async () => {
  // failing channel; event starts at NOW + 60 min, reminder fires at NOW; advance now to each next_attempt_at and runAlarm
  expect(nextAttempts).toEqual([NOW + 60e3, NOW + 6*60e3, NOW + 21*60e3]);
  expect(sendCalls).toBe(4); expect(finalStatus).toBe('failed');
});
it('a reminder whose formatting throws ends failed instead of looping', async () => { /* event row with tzid 'Bad/Zone' → 4 alarm runs → failed */ });
it('marks late reminders', async () => { /* fire_at 10 min ago, event in future → message body starts with '(late) ' */ });
it('arms the alarm at the earliest pending work', async () => {
  // reminder at NOW+10min, source due at NOW+30min, maintenance done now, queue empty
  await h.rearm(); expect(await state.storage.getAlarm()).toBe(NOW + 10*60e3);
});
it('arms within a second while the reconcile queue is non-empty', async () => { /* enqueue 1 → getAlarm() === NOW + 1000 */ });
it('does not call setAlarm when the target is unchanged', async () => { /* spy on storage.setAlarm across two rearm() calls → 1 call */ });
it('a permanently failing maintenance retries hourly, not every second', async () => {
  // runMaintenance throws (stub); runAlarm → getAlarm() >= NOW + 3_600_000 - 1 (when nothing else is due); meta:last_maintenance_error set
});
it('a top-level error applies the exponential error floor and resets on success', async () => {
  // force processQueue to throw via a broken sql stub: streak 1 → alarm >= NOW + 1 min; streak 2 → >= +2 min; ... capped at +15 min; clean run → streak 0
});
it('maintenance queues recurring series and purges 90-day-old deliveries', async () => { /* ... */ });
```
Also include one test verifying `runDurableObjectAlarm(stub)` executes `Hub.alarm()` end-to-end.

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement**; `Hub.alarm()` calls `runAlarm(this, { syncOne })` where `syncOne` is wired in Task 8. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: dispatch reminders from a self-arming alarm with error isolation`

---

### Task 8: Source sync

**Files:** Create `src/sources/sync.ts`; Modify `src/hub.ts` (wire `syncOne`); Test `test/sources/sync.test.ts`

**Interfaces:**
- Consumes: `parseIcs`, `IcsFormatError` (Task 3); `upsertEvent`, `deleteSeries`, `getSeries`, `enqueueSeries`, `processQueue` (Task 5); `formatSourceAlert` (Task 6).
- Produces: `MAX_ICS_BYTES = 5 * 1024 * 1024`; `pickDueSource(sql, now): SourceRow | null` (enabled sources never synced or with `last_sync_at + interval_min * 60_000 <= now`, oldest due first); `syncSource(hub: Hub, source: SourceRow): Promise<void>` (**never throws**); `getSource(sql, id): SourceRow | null`.

Sync rules (Spec §7.5):
- Send `If-None-Match` and `If-Modified-Since` when stored; HTTP `304` → `consecutive_failures = 0`, `last_error = null`, `last_result = 'unchanged'`.
- Enforce `MAX_ICS_BYTES` via `Content-Length` and while streaming (count bytes, cancel the reader on overflow) → error `'feed too large (> 5 MB)'`.
- **The whole body of `syncSource` runs inside one `try/catch/finally`.** Failure = network throw, non-2xx (`'HTTP <status>'`), oversize, `IcsFormatError` (`'not a calendar (no VCALENDAR)'`), **or any other exception** (`String(e)`): `consecutive_failures += 1`, `last_error`, events/reminders untouched; when the counter becomes exactly `3`, send `formatSourceAlert` via `deps.channel()` and append a `deliveries` row (`kind = 'source_alert'`; the alert's own failure is only logged). **`finally` always writes `last_sync_at = now`** (Spec Invariant 2).
- On valid parse, inside `transactionSync`:
  - Upsert each parsed row as `origin: 'ics'`, `seriesKey: ics:<source.id>:<uid>`, `leadMinutes: null`; collect changed `seriesKey`s and the set of present `(seriesKey, recurrenceId ?? '')` identities.
  - Prune rows of this source absent from the feed: whole series → `deleteSeries`; stale overrides of a surviving series → delete the row and mark the series changed.
  - `enqueueSeries(changed ∪ pruned-override series, 'sync', now)` — no inline reconciliation.
  - Persist `etag` / `last_modified`, `consecutive_failures = 0`, `last_error = null`, `last_result = 'ok: N events' + (warnings.length ? '; ' + warnings.join('; ') : '')`.

- [ ] **Step 1: Write the failing tests** (fake `deps.fetch`; `drain(h)` = call `processQueue` until `queueLength` is 0)

```ts
it('imports a feed, queues its series, and creates reminders once drained', async () => { /* weekly feed → events rows, queueLength 1, drain → pending reminders, last_result 'ok: 1 events' */ });
it('sends conditional headers and handles 304', async () => { /* second sync: request has If-None-Match "abc"; 304 → 'unchanged' */ });
it('removes events that disappear upstream and their pending reminders', async () => { /* ... */ });
it('prunes a stale override and restores the parent slot', async () => { /* override removed upstream → parent occurrence back at 09:00 after drain */ });
it('suppresses a cancelled instance', async () => { /* cancelled-instance fixture → no reminder for 2026-03-09 after drain */ });
it('reschedules a moved event', async () => { /* DTSTART changes → reminder fire_at changes after drain */ });
it('treats an HTML 200 page as failure and keeps existing events', async () => {
  // first sync ok, second returns '<!doctype html>...' with 200
  expect(eventsCount).toBe(1); expect(source.consecutiveFailures).toBe(1); expect(source.lastError).toMatch(/not a calendar/i);
});
it('rejects feeds over 5 MB', async () => { /* Content-Length 6_000_000 → failure 'feed too large (> 5 MB)' */ });
it('an unexpected exception counts as a failure and still advances last_sync_at', async () => {
  // fake fetch returns a body whose reader throws TypeError → consecutive_failures 1, last_sync_at = NOW, syncSource resolves
  // then Hub.rearm() → next alarm for this source is NOW + interval, not NOW + 1 s
});
it('alerts exactly once on the third consecutive failure', async () => { /* 4 failures → 1 alert message */ });
it('reports unknown TZID and RDATE warnings in last_result', async () => { /* unknown-tzid + rdate fixtures */ });
it('pickDueSource honors interval and enabled', () => { /* ... */ });
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement**; wire `syncOne = async h => { const s = pickDueSource(h.sql, h.deps.now()); if (s) await syncSource(h, s); }` inside `Hub`. **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: sync ICS sources with conditional requests and failure alerts`

---

### Task 9: Auth primitives

**Files:** Create `src/auth/crypto.ts`, `src/auth/cookies.ts`, `src/auth/session.ts`, `src/auth/recovery.ts`; Modify `src/http/app.ts` (middleware); Test `test/auth/*.test.ts`

**Interfaces:**
- Produces:
```ts
// crypto.ts
randomToken(bytes = 32): string;                                   // base64url
sha256Hex(s: string): Promise<string>;
signValue(payload: object, secret: string): Promise<string>;       // base64url(json).base64url(hmac-sha256)
verifyValue<T>(token: string, secret: string, now?: number): Promise<T | null>; // null on invalid sig or payload.exp < now
timingSafeEqualStr(a: string, b: string): Promise<boolean>;        // compares SHA-256 digests in constant time
// cookies.ts
parseCookies(header: string | null): Record<string, string>;
cookie(name: string, value: string, maxAgeSec: number): string;    // `${name}=${value}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=${maxAgeSec}`
// session.ts  (scope: 'full' | 'recovery')
SESSION_COOKIE = '__Host-sid'; SESSION_TTL_MS = 30 * 24 * 3600e3;
createSession(sql, scope, now): Promise<{ token: string; setCookie: string }>;
readSession(sql, cookieHeader, now): Promise<{ idHash: string; scope: 'full' | 'recovery' } | null>;  // extends expiry when last_extended_at older than 1 day
upgradeSession(sql, idHash): void; revokeSession(sql, idHash): void; revokeAllSessions(sql): void;
// recovery.ts
checkRecoveryCode(sql, env: Env, code: string, now: number): Promise<'ok' | 'wrong' | 'locked'>;
```

Middleware & rate-limiting rules:
- In `buildApp`: reject non-`GET`/`HEAD` requests under `/auth/*` and `/api/admin/*` with `403 { error: 'bad origin' }` unless `req.headers.get('Origin') === env.ORIGIN`.
- `requireSession('full' | 'any')`: missing/expired session → `401 { error: 'unauthenticated' }`; `recovery` session hitting a `'full'` route → `403 { error: 'register a passkey first' }`.
- `checkRecoveryCode`: if `meta:recovery_locked_until > now` return `'locked'`. On mismatch (`!timingSafeEqualStr`), insert `now` into `auth_failures`; if count within `[now - 3_600_000, now]` reaches `5`, set `meta:recovery_locked_until = now + 3_600_000`. On match, delete rows from `auth_failures` and return `'ok'`.

- [ ] **Step 1: Write the failing tests** — sign/verify round-trip, tampered token → `null`, expired → `null`; cookie formatting exact match `'__Host-sid=abc; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=2592000'`; session create → read → revoke → `null`; sliding extension after 2 days moves `expires_at`; expired session → `null`; recovery: correct → `'ok'`, 5 wrong → 5th returns `'wrong'`, 6th (even correct) returns `'locked'`, after `now + 1h` correct → `'ok'`; origin middleware: POST `/auth/logout` with `Origin: https://evil.example` → 403.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: add sessions, signed cookies, origin check and recovery rate limit`

---

### Task 10: Passkey and recovery flows

**Files:** Create `src/auth/passkey.ts`, `src/http/auth-routes.ts`; Modify `src/hub.ts` (default `deps.webauthn` = the real `@simplewebauthn/server` verifiers); Test `test/auth/passkey.test.ts`

**Interfaces:**
- Consumes: Task 9 helpers; `@simplewebauthn/server` (`generateRegistrationOptions`, `verifyRegistrationResponse`, `generateAuthenticationOptions`, `verifyAuthenticationResponse`).
- Produces: the default `HubDeps.webauthn` (type declared in Task 1) and the routes below. User verification (Spec §11.2): options use `userVerification: 'preferred'`; **both** `verifyRegistrationResponse` and `verifyAuthenticationResponse` are called with `requireUserVerification: false`.

| Route | Auth | Behavior |
|---|---|---|
| `GET /auth/state` | none | `{ setupRequired, authenticated, scope }`; `setupRequired` is `true` when zero `passkeys` match `rp_id = env.RP_ID` |
| `POST /auth/recovery` `{ code }` | none | `checkRecoveryCode`; `'ok'` → `createSession('recovery')`; `'wrong'` → `401 { error: 'wrong code' }`; `'locked'` → `429 { error: 'locked' }` |
| `POST /auth/passkey/register/options` | session any | `generateRegistrationOptions({ rpName: 'Reminder Hub', rpID: env.RP_ID, userName: 'owner', userID: <32-byte meta:webauthn_user_id created once>, excludeCredentials: current-RP keys, authenticatorSelection: { residentKey: 'required', userVerification: 'preferred' } })`; set challenge cookie `__Host-chal` = `signValue({ c, p: 'reg', exp: now + 120_000 })`, `Max-Age=120` |
| `POST /auth/passkey/register/verify` `{ name, response }` | session any | Verify challenge cookie (`p === 'reg'` and unexpired) and WebAuthn response with `expectedOrigin: env.ORIGIN`, `expectedRPID: env.RP_ID`, `requireUserVerification: false`; store `passkeys` row (`public_key` stored as `Uint8Array` / SQLite `BLOB`, `rp_id`, `name` ≤ 60 chars defaulting to `'Passkey'`); `upgradeSession`; clear `__Host-chal` |
| `POST /auth/passkey/login/options` | none | `generateAuthenticationOptions({ rpID: env.RP_ID, userVerification: 'preferred' })`; set `__Host-chal` (`p: 'auth'`, `Max-Age=120`) |
| `POST /auth/passkey/login/verify` `{ response }` | none | Verify challenge cookie (`p === 'auth'`); look up key by `response.id` AND `rp_id = env.RP_ID` (else `401 { error: 'unknown passkey' }`); verify with `credential: { id, publicKey: new Uint8Array(row.public_key), counter, transports }`, `expectedOrigin: env.ORIGIN`, `expectedRPID: env.RP_ID`, `requireUserVerification: false`; update `counter` and `last_used_at`; `createSession('full')`; clear `__Host-chal` |
| `POST /auth/logout` / `POST /auth/logout-all` | session any | Revoke current session / all sessions and clear `__Host-sid` |

Invalid, wrong-purpose, or expired challenge cookies return `400 { error: 'challenge expired' }`. Registering while already holding a `full` session adds an additional passkey.

- [ ] **Step 1: Write the failing tests** (fake verifiers return `{ verified: true, registrationInfo: { credential: { id: 'cred1', publicKey: new Uint8Array([1,2]), counter: 0, transports: ['internal'] } } }` and capture verifier inputs)

```ts
it('first-time setup requires the recovery code', async () => {
  expect((await get('/auth/state')).setupRequired).toBe(true);
  expect((await post('/auth/passkey/register/options', {})).status).toBe(401);
  const s = await post('/auth/recovery', { code: 'test-recovery-code' });   // recovery session
  expect((await post('/api/admin/settings', {}, s.cookie)).status).toBe(403);  // full scope required
  await registerPasskey(s.cookie, 'Phone');
  expect((await get('/auth/state', s.cookie))).toMatchObject({ setupRequired: false, scope: 'full' });
});
it('passes RP ID, origin, and requireUserVerification=false to both verifiers', async () => { /* expectedRPID 'localhost', expectedOrigin 'http://localhost:8787' on register and login */ });
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
// tokens.ts — raw format: 'rh_' + randomToken(32); stored as sha256Hex; prefix = first 10 chars
createToken(sql, name: string, now): Promise<{ id: number; token: string; prefix: string }>;
verifyBearer(sql, authHeader: string | null, now): Promise<number | null>;   // returns token id and updates last_used_at
listTokens(sql): { id: number; name: string; prefix: string; createdAt: number; lastUsedAt: number | null }[];
revokeToken(sql, id): void;
// validate.ts
type EventPayload = { title: string; start: string; end?: string; all_day?: boolean; timezone?: string;
  location?: string; description?: string; rrule?: string; reminders?: number[] };
validateEventPayload(body: unknown, userTz: string): { ok: true; fields: EventFields } | { ok: false; field: string; error: string };
```

Validation limits:
- `title`: 1–200 chars; `location` / `description`: ≤ 2,000 chars.
- `timezone`: optional; must pass `isValidZone`.
- `start`: ISO 8601 date (`all_day: true`) or date-time; if an offset or `Z` is present, convert via `utcToWall` into `timezone ?? userTz` (storing `tzid = timezone ?? userTz`); if naive (no offset), preserve wall-clock string with `tzid = timezone ?? null`.
- `end`: must be ≥ `start`; defaults to `start` (timed) or `start + 1 day` (all-day).
- `rrule`: optional; must parse cleanly via `ICAL.Recur.fromString`.
- `reminders`: optional array of ≤ 5 integers each in `[0, 10080]`.

Routes (all require `Authorization: Bearer <token>`; missing or invalid → `401 { error: 'invalid token' }`):
- `PUT /api/v1/events/:external_id` (`external_id` 1–200 chars matching `^[A-Za-z0-9._:-]+$`) → `400 { error, field }` | `200 { id, created: boolean }`; series key `api:<external_id>`; run `reconcileSeries` only when `changed`; call `rearm()`.
- `GET /api/v1/events?from=&to=` (ISO 8601; range ≤ 92 days, else `400`) → `{ events: [{ external_id: string | null, origin, title, start, end, all_day, location, truncated }] }` across all origins, built from `expandSeries(...).occurrences` per series (`start` / `end` formatted as ISO UTC; `truncated = status !== 'complete'`).
- `DELETE /api/v1/events/:external_id` → `204`, or `404 { error: 'not found' }`; scoped strictly to `api:<external_id>` by construction; call `rearm()`.

- [ ] **Step 1: Write the failing tests** — validation matrix (bounds, offset conversion `'2026-12-01T10:00:00+08:00'` with `timezone: 'Asia/Shanghai'` → `startLocal '2026-12-01T10:00:00'`, `tzid 'Asia/Shanghai'`, invalid `rrule` → `field: 'rrule'`); routes: missing token → 401; `PUT` creates with `created: true`, identical second `PUT` returns `created: false` leaving reminder count intact; `GET` range includes event; `DELETE` → 204 then 404; `GET` range > 92 days → 400; revoked token → 401.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: bearer-token API for events`

---

### Task 12: Admin API for the web UI

**Files:** Create `src/http/admin.ts`, `src/export/backup.ts`; Test `test/http/admin.test.ts`, `test/export/backup.test.ts`

**Interfaces:**
- Consumes: all domain, auth, sync, queue, and export modules. All routes enforce `requireSession('full')`; every mutating route ends with `hub.rearm()`. Single-series edits reconcile inline; bulk changes only enqueue (Spec §7.3).
- Produces (JSON payloads; numeric primary keys):

| Route | Behavior |
|---|---|
| `GET /api/admin/calendar?from&to` | Range ≤ 92 days; expands every series on demand with `expandSeries` (no `anchorBeforeUtc`, so it never writes); items `{ eventId, occKey, origin, sourceId, color, title, location, startUtc, endUtc, allDay, editable, deletable, truncated, reminders: [{ fireAt, status }] }` where `editable = origin === 'manual'`, `deletable = origin !== 'ics'`, `truncated = status !== 'complete'` |
| `GET/POST /api/admin/sources`, `PATCH/DELETE /api/admin/sources/:id`, `POST /api/admin/sources/:id/sync` | `POST` validates `https://` `url`, `name` 1–60 chars, `color` `#rrggbb`, `intervalMin` ≥ 15; `PATCH` affecting `defaultLeadMin` / `honorValarm` runs `enqueueWhere(sql, { sourceId }, 'source_settings', now)`; `DELETE` calls `deleteSeries` for each of its series and removes the source row; `/sync` triggers `syncSource` immediately and returns updated `SourceRow` |
| `POST /api/admin/events`, `PATCH/DELETE /api/admin/events/:id` | `manual` creation/edit validated with `validateEventPayload` and reconciled inline in the same `transactionSync`; `PATCH` on `ics` or `api` events → `409 { error: 'read-only' }`; `DELETE` permitted for `manual` and `api` (`409` for `ics`) |
| `GET/PUT /api/admin/settings` | `PUT` validates `timezone` (`isValidZone`), `defaultLeadMin` 0–10080, `allDayTime` `HH:MM`; changes to `timezone` or `allDayTime` run `enqueueWhere(sql, { floatingOrAllDay: true }, 'settings', now)`; changes to `defaultLeadMin` run `enqueueWhere(sql, {}, 'settings', now)`; response includes `{ queued: number }` |
| `GET /api/admin/queue` | `{ length: queueLength(sql) }` — polled by Settings while recomputing |
| `GET/PUT /api/admin/channel`, `POST /api/admin/channel/test` | Bark `{ server (`https://`, default `'https://api.day.app'`), deviceKey }`; `GET` masks `deviceKey` to its last 4 chars; `/test` dispatches `{ title: 'Reminder Hub', body: 'Test notification' }`, appends a `deliveries` row with `kind = 'test'`, and returns `SendResult` |
| `GET /api/admin/passkeys`, `PATCH /api/admin/passkeys/:id`, `DELETE /api/admin/passkeys/:id` | `GET` includes `stale: rp_id !== env.RP_ID`; `PATCH { name }` renames (1–60 chars); `DELETE` of the **last valid** key for `env.RP_ID` without `?confirm=1` → `409 { error: 'confirm_required', warning: 'This is your last passkey for this domain; you will need the recovery code to sign in again.' }` and nothing is deleted; with `?confirm=1` → deleted, `200`; deleting any other key → `200` |
| `GET/POST /api/admin/tokens`, `DELETE /api/admin/tokens/:id` | `POST` returns the raw `rh_...` token once |
| `GET /api/admin/log?limit=100` | `{ failedReminders, deliveries, sourceErrors, expansionWarnings, background }`: failed reminders pinned first; deliveries newest-first; sources with `consecutive_failures > 0`; parent rows with `expand_status !== 'complete'` or `expand_warning` set; `background = { alarmErrorStreak, lastAlarmError, lastMaintenanceError, queueLength }` |
| `GET /api/admin/export.json`, `GET /api/admin/export.ics` | Sets `Content-Disposition: attachment`; JSON `{ version: 1, exportedAt, settings, sources, events }` strips `passkeys`, `sessions`, and `api_tokens` and masks `deviceKey`; ICS builds one `VEVENT` per stored event row (`DTSTART`/`TZID`, `DURATION`, `RRULE`, `EXDATE`, `RECURRENCE-ID`) via `ical.js` |

- [ ] **Step 1: Write the failing tests** — unauthenticated → 401 across each route group; calendar returns colored items in range; source creation rejects `http://` URL and `intervalMin: 10`; **changing `timezone` from `UTC` to `Asia/Shanghai` returns `queued ≥ 1`, and after draining the queue a floating event's pending reminder has moved by −8 h while an explicit `tzid: 'Europe/Berlin'` event is untouched**; changing `allDayTime` to `07:30` moves all-day reminders after draining; creating a manual event returns with its reminder already present (inline); passkey `PATCH` renames; deleting the last valid passkey without `confirm` → 409 and the key still exists, with `?confirm=1` → deleted; log includes an expansion warning for a `too_dense` series; `PATCH` on an `ics` or `api` event → 409; channel `GET` masks `deviceKey`; `export.json` contains no raw `deviceKey`, `passkeys`, or `api_tokens`; `export.ics` round-trips through `parseIcs` to the same event count.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.
- [ ] **Step 5: Commit** — `feat: admin API for calendar, sources, settings and export`

---

### Task 13: Web app shell and auth screens

**Files:** Create `web/package.json`, `web/vite.config.ts`, `web/index.html`, `web/src/{main.tsx,api.ts,strings.ts,router.tsx,auth.tsx}`, `web/src/pages/{Setup.tsx,Login.tsx}`; Modify root `package.json` (`build:web` = `npm --prefix web run build`, `deploy` runs `npm run build:web && wrangler deploy`); Test `web/src/api.test.ts`

**Interfaces:**
- Consumes: `/auth/*` routes (Task 10).
- Produces: `api<T>(path: string, init?: { method?: string; body?: unknown }): Promise<T>` (`credentials: 'same-origin'`, throws `ApiError { status, error }`); `useAuth(): { state, refresh }`; routes `/setup`, `/login`, `/calendar`, `/sources`, `/events`, `/settings`, `/log`; auth guards: `setupRequired && scope !== 'recovery'` → `/setup`, unauthenticated → `/login`, `scope === 'recovery'` → `/setup` step 2.

Flows:
- **Setup:** Step 1 prompts for `RECOVERY_CODE`; Step 2 prompts for key name + `"Register passkey"` (`startRegistration({ optionsJSON })` via `@simplewebauthn/browser`), then defaults user time zone via `PUT /api/admin/settings { timezone: Intl.DateTimeFormat().resolvedOptions().timeZone }` before routing to `/calendar`.
- **Login:** Presents `"Sign in with passkey"` (`startAuthentication({ optionsJSON })`) plus a `"Use recovery code"` link routing to `/setup` step 1.
- Vite dev server proxies `/api` and `/auth` to `http://localhost:8787`; production build outputs to `web/dist`.
- `web/index.html` carries `<meta http-equiv="Content-Security-Policy" content="default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'">` (the Vite build emits no inline scripts; `frame-ancestors` is not honored in a meta tag, so it is omitted).

- [ ] **Step 1: Write the failing test** — `api()` with mocked `fetch`: 401 JSON `{ error: 'unauthenticated' }` throws `ApiError` with `status: 401`; JSON request sets `Content-Type: application/json`.
- [ ] **Step 2: Run** `npm --prefix web test` — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; `npm run build:web` generates `web/dist/index.html`.
- [ ] **Step 5: Commit** — `feat: web shell with passkey setup and login`

---

### Task 14: Web pages — calendar and management

**Files:** Create `web/src/pages/{Calendar.tsx,Sources.tsx,Events.tsx,Settings.tsx,Log.tsx}`, `web/src/calendar/grid.ts`, `web/src/components/{EventForm.tsx,EventDetail.tsx}`; Test `web/src/calendar/grid.test.ts`

**Interfaces:**
- Consumes: `/api/admin/*` (Task 12), `api()` (Task 13).
- Produces: `monthGrid(year: number, month: number /*1-12*/): string[][]` — 6 × 7 matrix of `YYYY-MM-DD`, Monday-first; `weekDays(date: string): string[]` — 7 Monday-first dates; format timestamps with `Intl.DateTimeFormat(undefined, { timeZone: settings.timezone })`.

UI coverage (matching Spec §13):
- **Calendar:** Month/week toggle, prev/next/today controls, source-colored occurrence blocks, deep-link highlight (`?event=<id>`), a hint on `truncated` series, and `EventDetail` drawer showing reminder statuses plus edit/delete for `manual` events, delete-only for `api` events, and read-only for `ics` events.
- **Sources:** Feed list displaying `last_result` and warnings, highlighting failing sources in red, with add/edit/toggle/delete and `"Sync now"`.
- **Events:** Manage `manual` events (`EventForm`), delete `api` events, and view `ics` events read-only.
- **Settings:** Time zone picker (`Intl.supportedValuesOf('timeZone')`), default lead minutes, all-day reminder time (after saving, poll `GET /api/admin/queue` every 2 s and show "Recomputing reminders… N left" until 0), Bark configuration + `"Send test"`, passkey list (stale badges, add key, rename inline, delete — on `409 confirm_required` show the returned warning in a confirm dialog and retry with `?confirm=1`), API token generator (one-time copyable token display), and JSON/ICS export buttons.
- **Log:** Pinned & highlighted failed reminders at top, chronological deliveries below, then panels for failing sources, expansion warnings, and background errors (alarm error streak, last alarm error, last maintenance error, queue length).

- [ ] **Step 1: Write the failing tests** — `monthGrid(2026, 11)[0][0] === '2026-10-26'`, `monthGrid(2026, 11)[5][6] === '2026-12-06'`, 7 cells per row; `weekDays('2026-11-19')` → `'2026-11-16'…'2026-11-22'`.
- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS; `npm run build:web` succeeds.
- [ ] **Step 5: Commit** — `feat: calendar, sources, events, settings and log pages`

---

### Task 15: End-to-end smoke test, CPU benchmark, and deploy docs

**Files:** Create `e2e/smoke.spec.ts`, `playwright.config.ts`, `.dev.vars.example`, `README.md`

**Interfaces:**
- Exercises the full Worker + SPA bundle under `wrangler dev` (`webServer` in `playwright.config.ts`: `npm run build:web && wrangler dev --port 8787`, with `.dev.vars` providing `RECOVERY_CODE=e2e-code` and `SESSION_SECRET=e2e-secret`).

- [ ] **Step 1: Write the smoke test** — Chromium with CDP virtual authenticator:
  `const cdp = await page.context().newCDPSession(page); await cdp.send('WebAuthn.enable'); await cdp.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true } })`.
  End-to-end flow: visit `/` → redirect `/setup` → submit `e2e-code` → register passkey `"E2E"` → land on `/calendar` → create manual event tomorrow at `10:00` → confirm visible in week view → log out → `/login` → sign in with passkey → `/calendar` visible.
- [ ] **Step 2: Run** `npx playwright test` — Expected: PASS.
- [ ] **Step 3: CPU check (Spec §3, §20)** — Deploy to a Free-plan Worker, subscribe to your largest real ICS feed, click `"Sync now"`, and measure invocation CPU via `wrangler tail --format json` (`cpuTime`) or the Cloudflare dashboard. Record the measured CPU time in `README.md`; if it consistently exceeds 10 ms for that feed size, document that the Workers Paid plan is required (no code change needed).
- [ ] **Step 4: Write `README.md`** — Prerequisites; `npm i`; pinning `RP_ID` / `ORIGIN` once in `wrangler.jsonc` (custom domain or `<name>.<account>.workers.dev`) and domain-change implications; setting secrets (`wrangler secret put RECOVERY_CODE` / `SESSION_SECRET` using `openssl rand -base64 32`); `npm run deploy`; initial bootstrap; registering additional passkeys; API `curl` example (`PUT /api/v1/events/my-id` with `Authorization: Bearer rh_...`); domain-change recovery steps; JSON/ICS backups; measured CPU telemetry.
- [ ] **Step 5: Final verification** — Run `npm test && npm run typecheck && npm --prefix web test && npm run build:web && npx playwright test` and confirm all pass.
- [ ] **Step 6: Commit** — `docs: deployment guide and end-to-end smoke test`
