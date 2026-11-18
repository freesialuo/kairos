#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

if [[ ${WORKERS_CI:-0} == 1 ]] && grep -Fq CHANGE-ME wrangler.toml; then
    echo "Set RP_ID and ORIGIN in wrangler.toml to your production hostname and HTTPS origin before deploying." >&2
    exit 1
fi

readonly wasm_target=wasm32-unknown-unknown
readonly trunk_version=0.22.2
readonly worker_build_version=0.9.0
readonly tools_dir="$PWD/target/cloudflare-tools"
readonly trunk_dir="$tools_dir/trunk-$trunk_version"
export PATH="$trunk_dir:$tools_dir/bin:${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

if ! command -v rustup >/dev/null; then
    curl --fail --silent --show-error --location --retry 3 https://sh.rustup.rs |
        sh -s -- -y --profile minimal --default-toolchain none --no-modify-path
fi

rustup toolchain install stable --no-update --no-self-update --profile minimal \
    --component clippy,rustfmt --target "$wasm_target"
# worker-build --panic-unwind invokes cargo +nightly and rebuilds std.
rustup toolchain install nightly --no-update --no-self-update --profile minimal \
    --component rust-src --target "$wasm_target"

if [[ $(trunk --version 2>/dev/null || true) != "trunk $trunk_version" ]]; then
    case "$(uname -s)/$(uname -m)" in
        Linux/x86_64)
            target=x86_64-unknown-linux-musl
            digest=4a90216d93c553f3d8b7401fce0dee588e0fb1eb1c6a379ebac81fabe820f25b
            ;;
        Linux/aarch64)
            target=aarch64-unknown-linux-musl
            digest=3485963cc854763d098a9702b5c98834fa7bfed7240a457ef00a5612b403c9e1
            ;;
        Darwin/arm64)
            target=aarch64-apple-darwin
            digest=84238ecc7def5ece23c257a392789ead196121adf4e1e7c25302a3d9c475275a
            ;;
        Darwin/x86_64)
            target=x86_64-apple-darwin
            digest=2d8fadad6f9ef3fd363474f6eaeb0bb3b92a3e17f4e4c06fa1b786184189fa2e
            ;;
        *)
            echo "Install Trunk $trunk_version on PATH for this platform." >&2
            exit 1
            ;;
    esac
    mkdir -p "$trunk_dir"
    archive="$trunk_dir/trunk-$target.tar.gz"
    curl --fail --silent --show-error --location --retry 3 --output "$archive" \
        "https://github.com/trunk-rs/trunk/releases/download/v$trunk_version/trunk-$target.tar.gz"
    if command -v sha256sum >/dev/null; then
        printf '%s  %s\n' "$digest" "$archive" | sha256sum --check
    else
        printf '%s  %s\n' "$digest" "$archive" | shasum -a 256 --check
    fi
    tar -xzf "$archive" -C "$trunk_dir" trunk
fi

if [[ $(worker-build --version 2>/dev/null || true) != "$worker_build_version" ]]; then
    # workers-rs does not publish a prebuilt worker-build for this release.
    cargo +stable install --locked --debug --bin worker-build --version "$worker_build_version" \
        --root "$tools_dir" worker-build
fi

# Both tools select the wasm-bindgen version from Cargo.lock.
(cd crates/kairos-web && RUSTUP_TOOLCHAIN=stable trunk build --release --locked)
worker-build --release --panic-unwind --out-dir ../../build crates/kairos-worker --locked

KAIROS_USE_PREBUILT=1 node scripts/build-worker.mjs
