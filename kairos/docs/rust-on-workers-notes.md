# Rust on Cloudflare Workers

## Worker build and bundle

The production dry run passed with Wrangler 4.164.0, `worker-build` 0.9.0, and Rust `1.102.0-nightly (59cbdee2e 2026-11-17)`. `worker-build` resolves `--out-dir` relative to the crate, so the build uses `../../build` to put the entry point at `build/worker/shim.mjs`. The end-to-end environment has its own output directory so its test hooks stay out of the production bundle.

```sh
rustup toolchain install nightly --profile minimal --component rust-src --target wasm32-unknown-unknown
cargo install --locked --version 0.9.0 worker-build
worker-build --release --panic-unwind --out-dir ../../build crates/kairos-worker
npx wrangler deploy --dry-run --outdir build/dry --env ''
```

Wrangler read one placeholder asset from `crates/kairos-web/dist`. The optimized `build/index_bg.wasm` was 541,570 bytes (528.88 KiB). Wrangler reported 570.92 KiB total upload size and 214.68 KiB gzip. These measurements are for the adapter spike. The Worker was bundled locally.

`worker-build` 0.9.0 explicitly invokes `cargo +nightly` for `--panic-unwind`, so `rust-toolchain.toml` does not pin the Worker compiler. The first unwind build required the Durable Object to store its configuration error as a string and mark the migration flag unwind-safe. The flag changes only after migration succeeds. The Worker’s release profile preserves symbol metadata through wasm-bindgen so it can locate `__stack_pointer` and read `target_features`. `wasm-opt` strips debug metadata after wasm-bindgen. The final dry run had no missing-metadata warnings. Recovery after a hard Wasm trap remains unverified. [worker-build toolchain source](https://docs.rs/crate/worker-build/0.9.0/source/src/build/target.rs)

## Storage and entropy

`worker` 0.9.0 supplies `Storage::as_raw()`, so `DoDb` can bind `transactionSync` on the underlying JavaScript storage object. A scoped synchronous callback keeps the original Rust outcome and throws on an error so Workers rolls back the writes. The adapter resumes a Rust panic after `transactionSync` returns control. `DoCommit` awaits the SDK’s `Storage::sync()`. [workers-rs storage source](https://docs.rs/crate/worker/0.9.0/source/src/durable.rs) · [Cloudflare SQLite storage API](https://developers.cloudflare.com/durable-objects/api/sqlite-storage-api/)

`getrandom` 0.4.3 builds for `wasm32-unknown-unknown` with the `wasm_js` Cargo feature. The commit probe exercises the WebCrypto backend in the local Workers runtime. No `getrandom_backend` Rust flag or `.cargo/config.toml` change was needed. [getrandom WebAssembly backend](https://docs.rs/getrandom/0.4.3/getrandom/#webassembly-support)

The local Wrangler lifecycle test verified that a Rust panic rolls back a SQLite write and leaves the Durable Object able to handle another transaction. It also verified explicit rollback, a successful storage sync, alarm persistence across restart, rejection of an invalid alarm time, and chunked fetches at and above the 5 MiB cap.
