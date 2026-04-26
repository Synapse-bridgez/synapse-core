use super::run_cmd;
use clap::Args;

/// Arguments for `cargo xtask lint`
#[derive(Args)]
pub struct LintArgs {
    /// Skip `cargo fmt` check.
    #[arg(long)]
    pub skip_fmt: bool,

    /// Skip `cargo clippy`.
    #[arg(long)]
    pub skip_clippy: bool,

    /// Skip `cargo audit`.
    #[arg(long)]
    pub skip_audit: bool,

    /// Apply `cargo fmt` fixes instead of just checking.
    #[arg(long)]
    pub fix: bool,
}

pub fn run(args: LintArgs) -> anyhow::Result<()> {
    println!("==> synapse-core lint");

    if !args.skip_fmt {
        run_fmt(args.fix)?;
    }

    if !args.skip_clippy {
        run_clippy()?;
    }

    if !args.skip_audit {
        run_audit()?;
    }

    println!("\n✓ All lint checks passed.");
    Ok(())
}

fn run_fmt(fix: bool) -> anyhow::Result<()> {
    println!("\n-- cargo fmt --");
    if fix {
        run_cmd("cargo", &["fmt", "--all"])?;
    } else {
        run_cmd("cargo", &["fmt", "--all", "--", "--check"])?;
    }
    Ok(())
}

fn run_clippy() -> anyhow::Result<()> {
    println!("\n-- cargo clippy --");
    run_cmd(
        "cargo",
        &[
            "clippy",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    Ok(())
}

fn run_audit() -> anyhow::Result<()> {
    println!("\n-- cargo audit --");
    // cargo-audit must be installed: `cargo install cargo-audit`
    run_cmd("cargo", &["audit"])?;
    Ok(())
}
