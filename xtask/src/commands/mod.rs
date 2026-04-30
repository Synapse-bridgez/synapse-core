pub mod lint;
pub mod release;
pub mod setup;
pub mod test;

use std::process::{Command, ExitStatus};

/// Run a shell command, streaming its output to stdout/stderr.
/// Returns an error if the command exits with a non-zero status.
pub fn run_cmd(program: &str, args: &[&str]) -> anyhow::Result<ExitStatus> {
    println!("» {} {}", program, args.join(" "));
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to spawn `{program}`: {e}"))?;

    if !status.success() {
        anyhow::bail!("`{} {}` exited with {}", program, args.join(" "), status);
    }
    Ok(status)
}
