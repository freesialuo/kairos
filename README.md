# Kairos

Kairos is a single-user calendar and reminder app for Cloudflare Workers. It imports ICS feeds, accepts events through an authenticated HTTP API, and sends reminders through Bark. The browser app and the Worker share Rust types. One SQLite-backed Durable Object owns the calendar, sessions, and reminder queue.

## Build and test locally

Run the commands below from the directory containing this README. Install Rust with rustup, Node.js with npm, and Chrome or Chromium. The workspace selects stable Rust for the core and browser app. Worker builds with panic recovery also require nightly and its Rust source component.

```sh
rustup target add wasm32-unknown-unknown --toolchain stable
rustup toolchain install nightly --profile minimal --component rust-src --target wasm32-unknown-unknown
cargo install --locked --version 0.22.2 trunk
cargo install --locked --version 0.2.130 wasm-bindgen-cli
cargo install --locked --version 0.9.0 worker-build
cargo xtask test
cargo xtask build
cargo xtask e2e
```

`test` runs the native workspace suite, core WebAssembly tests under Node, and Clippy. `build` compiles the browser app and bundles the release Worker with a local Wrangler deployment dry run. `e2e` starts Wrangler on localhost:8799, exercises the actual storage and alarm adapters, then uses Chrome with a virtual passkey to register, sign in, create an event, and verify delivery to a local Bark stand-in. Set `KAIROS_CHROME_PATH` if Chromium is installed outside its usual location. The first build downloads the locked dependencies and build tools.

## First deployment

1. Choose the HTTPS hostname where the browser app will actually be served. In `wrangler.toml`, set the production `RP_ID` to that hostname and `ORIGIN` to its exact HTTPS origin. Leave the local e2e environment on localhost. Configure the corresponding Worker route or custom domain in Cloudflare if needed.
2. Authenticate Wrangler to the intended Cloudflare account. Store a strong recovery code and an independent, random 32-byte session secret. Generate the session secret with `openssl rand -hex 32`. Paste each value into its Wrangler prompt and keep the recovery code in your password manager.
3. Deploy and open `/setup` on the configured hostname. Enter the recovery code, register your first passkey, and review the time zone under **Settings**.

```sh
npx --yes wrangler@4.164.0 login
npx --yes wrangler@4.164.0 secret put RECOVERY_CODE --env ''
npx --yes wrangler@4.164.0 secret put SESSION_SECRET --env ''
cargo xtask deploy
```

## Use and operate Kairos

- **Settings** controls the display time zone, default reminder lead, all-day reminder time, Bark endpoint and device key, passkeys, API tokens, and exports. Use the channel test to check the real Bark device. A new API token is shown once.
- **Sources** accepts HTTPS ICS feeds. Each source has a refresh interval and independent visibility and notification settings. **Events** creates one-off and recurring manual events. **Calendar** displays imported, manual, and API events together.
- API clients send `Authorization: Bearer <token>`. Use `PUT /api/v1/events/{external_id}` to create or replace an API event, `DELETE` on the same path to remove it, and `GET /api/v1/events?from=…&to=…` to list occurrences. Use explicit-offset timestamps for timed instants. See the architecture document for payloads, limits, and ownership rules.
- **Log** shows delivery attempts, source failures, recurrence warnings, and background scheduler errors. A six-hour watchdog rearms a missing alarm. Use `npx --yes wrangler@4.164.0 tail --format json` for platform logs. Measure CPU during a sync of your largest feed before choosing a Workers plan.
- After changing the domain, update both production variables, redeploy, then use the recovery flow on the new origin to register a new passkey. Browser passkeys are scoped to the relying-party domain.

[Architecture](docs/2026-11-18-rust-architecture.md) · [Implementation and verification status](docs/superpowers/plans/2026-11-18-reminder-hub-rust.md) · [Worker measurements](docs/rust-on-workers-notes.md)
