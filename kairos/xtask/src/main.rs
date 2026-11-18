use std::{
    env, io,
    process::{Command, ExitCode},
};

fn cargo(args: &[&str]) -> io::Result<bool> {
    let executable = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Ok(Command::new(executable).args(args).status()?.success())
}

fn main() -> io::Result<ExitCode> {
    let mut args = env::args().skip(1);
    if args.next().as_deref() != Some("test") || args.next().is_some() {
        use std::io::Write;
        writeln!(io::stderr(), "Usage: cargo xtask test")?;
        return Ok(ExitCode::FAILURE);
    }
    let success = cargo(&["test", "--workspace"])?
        && cargo(&[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ])?;
    Ok(if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
