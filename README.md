# Kairos

Kairos is a single-user calendar and reminder app for Cloudflare Workers. It imports ICS feeds, accepts events through an authenticated HTTP API, and sends reminders through Bark. The browser app and the Worker share Rust types. One SQLite-backed Durable Object owns the calendar, sessions, and reminder queue.

## Deploy to Cloudflare

[Connect GitHub to Cloudflare Workers Builds](docs/deployment.md#deploy-automatically-from-github) to deploy each push to `main`, or [deploy from a local checkout with Wrangler](docs/deployment.md#1-get-the-repository-and-build-tools). Both paths cover hostname setup, runtime secrets, passkey registration, and a test reminder on your Bark device.

## Build and test locally

Run these commands from the repository root after installing the tools listed in the deployment guide.

| Command | What it checks |
| --- | --- |
| `cargo xtask build` | Builds the browser and Worker release bundles and runs a local Wrangler deployment dry run. |
| `cargo xtask test` | Runs the native workspace tests, core WebAssembly tests under Node, and Clippy. |
| `cargo xtask e2e` | Starts local Wrangler on port 8799 and uses Chrome with a virtual passkey to check setup, sign-in, and delivery to a local Bark stand-in. |

Chrome or Chromium is required for `e2e`. Set `KAIROS_CHROME_PATH` if your browser is installed outside its usual location.

## Use and operate Kairos

- **Settings** controls the display time zone, default reminder lead, all-day reminder time, Bark endpoint and device key, passkeys, API tokens, and exports. Use the channel test to check the real Bark device. A new API token is shown once.
- **Sources** accepts HTTPS ICS feeds. Each source has a refresh interval and independent visibility and notification settings. **Events** creates one-off and recurring manual events. **Calendar** displays imported, manual, and API events together.
- API clients send `Authorization: Bearer <token>`. Use `PUT /api/v1/events/{external_id}` to create or replace an API event, `DELETE` on the same path to remove it, and `GET /api/v1/events?from=…&to=…` to list occurrences. Use explicit-offset timestamps for timed instants. See the architecture document for payloads, limits, and ownership rules.
- **Log** shows delivery attempts, source failures, recurrence warnings, and background scheduler errors. A six-hour watchdog rearms a missing alarm. Use `npx --yes wrangler@4.164.0 tail --format json` for platform logs. Measure CPU during a sync of your largest feed before choosing a Workers plan.
- After changing the domain, update both production variables, redeploy, then use the recovery flow on the new origin to register a new passkey. Browser passkeys are scoped to the relying-party domain.

[Architecture](docs/2026-11-18-rust-architecture.md) · [Implementation and verification status](docs/superpowers/plans/2026-11-18-reminder-hub-rust.md) · [Worker measurements](docs/rust-on-workers-notes.md)
