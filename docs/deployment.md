# Deploy Kairos to Cloudflare Workers

Kairos deploys as one Worker that serves the browser app and runs the API. Wrangler also provisions the SQLite-backed `Hub` Durable Object and schedules a six-hour watchdog. The Hub stores your calendar, sign-in data, and reminder queue. Cloudflare supports SQLite-backed Durable Objects on Workers Free and Workers Paid. [Durable Object storage](https://developers.cloudflare.com/durable-objects/best-practices/access-durable-objects-storage/)

## 1. Get the repository and build tools

Run these commands in Bash or Zsh from macOS or Linux. Install Git, curl, Rust through rustup, and Node.js with npm. Use a supported Node.js release at or above 22.12, the minimum for the pinned Wrangler version. A first build needs network access to download dependencies and build tools. [Rust installation](https://rustup.rs/) · [Wrangler installation](https://developers.cloudflare.com/workers/wrangler/install-and-update/)

- **macOS.** Run `xcode-select --install` if you need the Xcode Command Line Tools. [Rust build prerequisites](https://doc.rust-lang.org/stable/book/ch01-01-installation.html)
- **Ubuntu or Debian.** Run `sudo apt-get update` and `sudo apt-get install git curl build-essential pkg-config libssl-dev`. [Rust build prerequisites](https://doc.rust-lang.org/stable/book/ch01-01-installation.html) · [OpenSSL build prerequisites](https://docs.rs/openssl/latest/openssl/#automatic)

```sh
git clone https://github.com/freesialuo/kairos.git
cd kairos

rustup toolchain install stable --profile minimal --component clippy --component rustfmt --target wasm32-unknown-unknown
rustup toolchain install nightly --profile minimal --component rust-src --target wasm32-unknown-unknown
cargo +stable install --locked --version 0.22.2 trunk
cargo +stable install --locked --version 0.2.130 wasm-bindgen-cli
cargo +stable install --locked --version 0.9.0 worker-build
```

Use the root of the existing checkout if you already cloned the repository. Keep Cargo's binary directory on `PATH`. The browser app builds with stable Rust and Trunk. `worker-build` uses nightly and `rust-src` to build the Worker with panic recovery. The `wasm-bindgen` CLI must match the version in `Cargo.lock`. [Worker build notes](rust-on-workers-notes.md)

```sh
cargo xtask build
```

This builds the browser assets and runs Wrangler's deployment dry run locally. A successful dry run bundles both parts of the app. For the native and WebAssembly test suites, run `cargo xtask test`. For the browser and storage smoke tests, install Chrome or Chromium and run `cargo xtask e2e`. Set `KAIROS_CHROME_PATH` if the tests cannot find your browser. [Local test details](../README.md#build-and-test-locally)

## 2. Select the Cloudflare account

```sh
npx --yes wrangler@4.164.0 login
npx --yes wrangler@4.164.0 whoami
```

Approve the sign-in in your browser and verify that Wrangler lists the account you intend to use. Use a terminal without Cloudflare API credentials for another account. If browser login cannot reach Wrangler's local callback, rerun `login` with `--device`. In the same terminal, set the account ID shown by `whoami` for the rest of the deployment. [Wrangler authentication](https://developers.cloudflare.com/workers/wrangler/commands/general/)

```sh
export CLOUDFLARE_ACCOUNT_ID='paste-the-intended-account-id-here'
```

In that account's **Workers & Pages** dashboard, check whether a Worker named `kairos` already exists. Deploying under an existing name updates that Worker. For a separate instance, choose an unused `name` at the top of `wrangler.toml` before deployment. [Account selection](https://developers.cloudflare.com/workers/wrangler/system-environment-variables/)

## 3. Set the public hostname

Choose the HTTPS hostname you will use to open Kairos. Passkeys are tied to the configured relying-party ID, and Kairos checks the browser's exact origin during sign-in and other writes.

### Use `workers.dev`

Open **Workers & Pages** and read **Your subdomain** in the intended account. If prompted, set up the account's `workers.dev` subdomain. With Worker name `kairos` and account subdomain `example-account`, the URL is `https://kairos.example-account.workers.dev`. [Cloudflare workers.dev setup](https://developers.cloudflare.com/workers/configuration/routing/workers-dev/)

In the top-level `[vars]` table in `wrangler.toml`, replace the two placeholder values with your actual hostname and origin.

```toml
[vars]
RP_ID = "kairos.example-account.workers.dev"
ORIGIN = "https://kairos.example-account.workers.dev"
```

### Use a custom domain

Choose an unused hostname in an active Cloudflare DNS zone in the same account. Deploying the route below makes the Worker the origin and lets Cloudflare provision DNS and TLS. [Cloudflare Custom Domains](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/)

Add these top-level keys in `wrangler.toml`, above `[build]`. Replace `calendar.example.com` with your hostname.

```toml
workers_dev = false
preview_urls = false
routes = [{ pattern = "calendar.example.com", custom_domain = true }]
```

Set the existing top-level `[vars]` to the same hostname.

```toml
[vars]
RP_ID = "calendar.example.com"
ORIGIN = "https://calendar.example.com"
```

For either hostname, `RP_ID` takes the hostname alone and `ORIGIN` takes `https://` followed by the hostname, with no trailing slash or path. Deploy the top-level configuration and keep `[env.e2e]` on localhost for tests.

## 4. Deploy with both secrets

Generate a strong random recovery code in your password manager and save it there. The code lets you register replacement passkeys. The command below accepts it without echoing it and generates an independent 32-byte session secret as 64 hex characters. It writes both values to a private temporary JSON file outside the repository and supplies that file to the first upload. Run it from the repository root after checking the account ID and hostname. [Worker secrets](https://developers.cloudflare.com/workers/configuration/secrets/#upload-secrets-alongside-code)

```sh
(
  set +x
  umask 077
  kairos_deploy_dir="$(mktemp -d /tmp/kairos-deploy.XXXXXXXX)" || exit 1
  kairos_secrets_file="$kairos_deploy_dir/secrets.json"
  printf 'Temporary directory %s\n' "$kairos_deploy_dir"
  printf 'Paste the recovery code and press Enter ' >&2
  IFS= read -r -s kairos_recovery_code || exit 1
  printf '\n' >&2
  if [ -z "$kairos_recovery_code" ]; then
    printf 'A recovery code is required.\n' >&2
    exit 1
  fi

  printf '%s' "$kairos_recovery_code" | node -e '
    const { randomBytes } = require("node:crypto");
    const { readFileSync } = require("node:fs");
    process.stdout.write(JSON.stringify({
      RECOVERY_CODE: readFileSync(0, "utf8"),
      SESSION_SECRET: randomBytes(32).toString("hex")
    }));
  ' > "$kairos_secrets_file" || exit 1
  unset kairos_recovery_code
  cargo xtask deploy --secrets-file "$kairos_secrets_file"
)
```

Wrangler prints the deployment URL and version ID on success. Check that the URL matches the origin you configured. If the upload fails after the JSON file was created, fix the reported issue and retry with `cargo xtask deploy --secrets-file /path/from/output/secrets.json`, using the actual temporary path. Move the temporary directory to your system Trash as soon as you finish with it. Keep the recovery code in your password manager and share neither the file nor its contents.

## 5. Verify the deployment and register your passkey

Use the actual origin from `wrangler.toml` in these commands.

```sh
KAIROS_ORIGIN='https://kairos.example-account.workers.dev'
curl -fsS "$KAIROS_ORIGIN/api/health"
curl -fsS "$KAIROS_ORIGIN/auth/state"
```

`/api/health` should return `{"ok":true}`. It exercises the Worker and the Hub's initialization. `/auth/state` should return HTTP 200 with `setup_required` set to `true` before you register the first passkey on that hostname.

1. Open `/setup` on the configured HTTPS origin in a passkey-capable browser. Paste your saved recovery code, select **Continue**, then select **Register passkey**. The browser prompts you to save the passkey and Kairos opens **Calendar**.
2. Log out and sign in with the passkey to verify the full sign-in flow. Under **Settings**, review your time zone and reminder defaults.
3. Under **Settings** → **Bark notifications**, enter your HTTPS Bark server and device key, select **Enable Bark**, and choose **Save channel**. Select **Send test notification** and confirm that your device receives **Kairos test**.
4. To verify scheduled delivery, go to **Events** → **Add event** and name it `Kairos deploy check`. Set **Starts** to 10 minutes from now in your calendar's time zone, choose **Custom** under **Reminder schedule**, enter `5` in **Minutes before the event**, and save. About five minutes before the event, your device should receive **⏰ Kairos deploy check**. Refresh **Log** and look under **Recent deliveries** for a **Sent** reminder entry at the expected time. The log identifies it by reminder key. A delayed notification may show a `(late)` prefix on your device.
5. If you import calendars, add an HTTPS ICS feed in **Sources** and verify the events in **Calendar**. Check **Log** after syncing your largest feed and review the CPU usage in Cloudflare before choosing a Workers plan. [Worker limits](https://developers.cloudflare.com/workers/platform/limits/)

## Update or troubleshoot Kairos

- **Deploy a new revision.** Update your checkout, review any configuration changes, and keep the account, Worker `name`, and `Hub` binding on the existing instance. Preserve the deployment's hostname settings in `wrangler.toml`. After testing, run `cargo xtask deploy` and recheck `/api/health` and passkey sign-in. Cloudflare retains existing secrets on a normal redeploy. [Worker secrets](https://developers.cloudflare.com/workers/configuration/secrets/)
- **Inspect a failed request.** From the repository root, run `npx --yes wrangler@4.164.0 tail --env '' --format pretty` and repeat the request. A health check failure with a Kairos initialization error suggests a missing secret or a malformed session secret. Verify the secret names with `npx --yes wrangler@4.164.0 secret list --env ''`. [Wrangler logs and secrets commands](https://developers.cloudflare.com/workers/wrangler/commands/workers/)
- **Fix an unreachable hostname.** Compare the configured origin with the deployed URL and inspect **Settings** → **Domains & Routes** for the Worker. For a new custom domain, check that the zone is active, the hostname has no conflicting DNS record, and Cloudflare has issued the certificate. [Custom Domain requirements](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/)
- **Fix passkey or recovery errors.** Compare the browser's address with `RP_ID` and `ORIGIN`. Complete the ceremony on the configured origin. When changing domains, update the route and both variables on the same Worker, redeploy, and use the recovery code on `/setup` at the new origin to register a passkey for that hostname.
- **Check versions before rolling back.** Use the Worker's **Deployments** tab to find the last working version. Check that it is compatible with the current database schema. A Worker rollback changes the deployed code and configuration while the Hub's data stays at its current state. Download the calendar export from **Settings** before significant updates if you need a copy of the calendar data. [Cloudflare rollback behavior](https://developers.cloudflare.com/workers/versions-and-deployments/rollbacks/)
