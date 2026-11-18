use std::{
    env, fs, io,
    path::Path,
    process::{Command, ExitCode},
};

fn cargo(args: &[&str]) -> io::Result<bool> {
    let executable = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Ok(Command::new(executable).args(args).status()?.success())
}

fn main() -> io::Result<ExitCode> {
    let mut args = env::args().skip(1);
    let action = args.next();
    let success = match (action.as_deref(), args.next()) {
        (Some("test"), None) => {
            cargo(&["test", "--workspace"])?
                && cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ])?
        }
        (Some("e2e"), None) => {
            let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/kairos-web/dist");
            fs::create_dir_all(&assets)?;
            let index = assets.join("index.html");
            if !index.exists() {
                fs::write(index, "<!doctype html><title>Kairos</title>")?;
            }
            cargo(&[
                "test",
                "-p",
                "e2e",
                "--features",
                "wrangler",
                "--",
                "--test-threads=1",
            ])?
        }
        _ => {
            use std::io::Write;
            writeln!(io::stderr(), "Usage: cargo xtask <test|e2e>")?;
            false
        }
    };
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
