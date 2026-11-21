mod commands;

use clap::{Parser, Subcommand};

/// Synapse-Core development task runner.
///
/// Run common development workflows with a single command.
/// All commands assume you are at the workspace root.
#[derive(Parser)]
#[command(name = "xtask", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start docker-compose services, run database migrations, and seed initial data.
    ///
    /// Gets a new developer environment ready to run in under 2 minutes.
    /// Requires: docker, docker-compose, sqlx-cli
    Setup(commands::setup::SetupArgs),

    /// Run unit tests and integration tests.
    ///
    /// Executes `cargo test` for all workspace members.
    /// Set DATABASE_URL before running integration tests.
    Test(commands::test::TestArgs),

    /// Run code quality checks: fmt, clippy, and cargo-audit.
    ///
    /// Requires: cargo-audit (`cargo install cargo-audit`)
    Lint(commands::lint::LintArgs),

    /// Build a release, run tests, create a git tag, and push.
    ///
    /// Requires a clean git working tree and the VERSION env var or --version flag.
    Release(commands::release::ReleaseArgs),
}

/// Redact the password portion of a database URL, if present.
///
/// `postgres://user:secret@host/db` becomes `postgres://user:***@host/db`.
/// URLs without a password are returned unchanged.
pub fn redact_database_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map(|i| authority_start + i)
        .unwrap_or(url.len());
    let authority = &url[authority_start..authority_end];

    let Some(at) = authority.rfind('@') else {
        return url.to_string();
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return url.to_string();
    };

    let mut redacted = String::with_capacity(url.len());
    redacted.push_str(&url[..authority_start]);
    redacted.push_str(&userinfo[..colon]);
    redacted.push_str(":***");
    redacted.push_str(&authority[at..]);
    redacted.push_str(&url[authority_end..]);
    redacted
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup(args) => commands::setup::run(args),
        Commands::Test(args) => commands::test::run(args),
        Commands::Lint(args) => commands::lint::run(args),
        Commands::Release(args) => commands::release::run(args),
    }
}
