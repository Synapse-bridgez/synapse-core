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

    /// Skip the migration safety lint (expand/contract checks).
    #[arg(long)]
    pub skip_migrations: bool,

    /// Skip the migration rollback rehearsal.
    #[arg(long)]
    pub skip_rollback: bool,

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

    if !args.skip_migrations {
        run_migration_lint()?;
    }

    if !args.skip_rollback {
        run_rollback_rehearsal()?;
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

/// Runs the migration safety checker over `migrations/`.
///
/// The checker flags obviously unsafe migrations (dropping/renaming columns
/// still referenced, adding non-nullable columns without defaults, and
/// long-running locks on large tables) and enforces the expand/contract
/// practice documented in `docs/migrations.md`. It is a fast, static pass
/// suitable for per-PR CI.
fn run_migration_lint() -> anyhow::Result<()> {
    println!("\n-- migration safety lint --");
    // The checker lives in `xtask` itself so it can run without extra tooling.
    run_cmd(
        "cargo",
        &[
            "run",
            "--quiet",
            "--package",
            "xtask",
            "--bin",
            "migration-lint",
            "--",
            "--migrations-dir",
            "migrations",
        ],
    )?;
    Ok(())
}

/// Rehearses the down path of every migration against a populated test
/// database.
///
/// The runner applies all migrations forward against an empty database, seeds
/// representative data, then applies each migration's down path (or its
/// documented equivalent) in reverse order, verifying that the schema returns
/// to a consistent state. It is designed to complete quickly enough for
/// per-PR CI by using an ephemeral database.
fn run_rollback_rehearsal() -> anyhow::Result<()> {
    println!("\n-- migration rollback rehearsal --");
    run_cmd(
        "cargo",
        &[
            "run",
            "--quiet",
            "--package",
            "xtask",
            "--bin",
            "migration-rollback",
            "--",
            "--migrations-dir",
            "migrations",
            "--seed",
            "xtask/fixtures/rollback_seed.sql",
        ],
    )?;
    Ok(())
}