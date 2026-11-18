use std::{
    env, fs, io,
    path::Path,
    process::{Command, ExitCode},
};

fn cargo(args: &[&str]) -> io::Result<bool> {
    let executable = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Ok(Command::new(executable).args(args).status()?.success())
}

fn web_build() -> io::Result<bool> {
    Ok(Command::new("trunk")
        .current_dir("crates/kairos-web")
        .args(["build", "--release"])
        .status()?
        .success())
}

fn build() -> io::Result<bool> {
    if !web_build()? {
        return Ok(false);
    }
    // The Wrangler build block defines the Worker compiler and its release flags.
    Ok(Command::new("npx")
        .args([
            "--yes",
            "wrangler@4.164.0",
            "deploy",
            "--dry-run",
            "--env",
            "",
            "--outdir",
            "build/dry",
        ])
        .status()?
        .success())
}

fn check_web_source(dir: &Path) -> io::Result<()> {
    for file in fs::read_dir(dir)? {
        let path = file?.path();
        if path.is_dir() {
            check_web_source(&path)?;
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && fs::read_to_string(&path)?.contains("inner_html")
        {
            return Err(io::Error::other(format!(
                "{} renders untrusted HTML",
                path.display()
            )));
        }
    }
    Ok(())
}

fn test() -> io::Result<bool> {
    check_web_source(Path::new("crates/kairos-web/src"))?;
    if !cargo(&["test", "--workspace", "--locked"])? {
        return Ok(false);
    }
    let runner = env::var_os("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER")
        .unwrap_or_else(|| "wasm-bindgen-test-runner".into());
    let wasm = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .env("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER", runner)
        .args([
            "test",
            "--locked",
            "-p",
            "kairos-core",
            "--target",
            "wasm32-unknown-unknown",
            "--lib",
        ])
        .status()?;
    if !wasm.success() {
        return Ok(false);
    }
    if !cargo(&[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--",
        "-D",
        "warnings",
    ])? {
        return Ok(false);
    }
    cargo(&[
        "clippy",
        "--locked",
        "--target",
        "wasm32-unknown-unknown",
        "-p",
        "kairos-web",
        "-p",
        "kairos-worker",
        "--",
        "-D",
        "warnings",
    ])
}

fn main() -> io::Result<ExitCode> {
    env::set_current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))?;
    let mut args = env::args().skip(1);
    let success = match (args.next().as_deref(), args.next()) {
        (Some("test"), None) => test()?,
        (Some("build"), None) => build()?,
        (Some("e2e"), None) => {
            web_build()?
                && cargo(&[
                    "test",
                    "--locked",
                    "-p",
                    "e2e",
                    "--features",
                    "wrangler,browser",
                    "--",
                    "--test-threads=1",
                ])?
        }
        (Some("deploy"), None) => {
            if fs::read_to_string("wrangler.toml")?.contains("CHANGE-ME") {
                return Err(io::Error::other(
                    "Set RP_ID and ORIGIN in wrangler.toml before deployment.",
                ));
            }
            if env::vars_os().any(|(k, v)| {
                k == "CARGO_FEATURE_TEST_HOOKS"
                    || (k == "RUSTFLAGS" || k == "CARGO_ENCODED_RUSTFLAGS")
                        && v.to_string_lossy().contains("test-hooks")
            }) {
                return Err(io::Error::other(
                    "Production deployment cannot enable test hooks.",
                ));
            }
            build()?
                && Command::new("npx")
                    .args(["--yes", "wrangler@4.164.0", "deploy", "--env", ""])
                    .status()?
                    .success()
        }
        _ => {
            eprintln!("Usage: cargo xtask <build|test|e2e|deploy>");
            false
        }
    };
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
