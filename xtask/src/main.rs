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

    /// Render or check the SDK/CLI/API compatibility matrix
    /// (COMPATIBILITY.toml -> docs/compatibility-matrix.md).
    CompatMatrix(commands::compat::CompatMatrixArgs),

    /// Audit indexes for low usage over a monitoring window.
    ///
    /// Identifies unused or rarely-used indexes that are not backing constraints,
    /// so operators can safely remove them and reduce write overhead.
    AuditIndexes(commands::audit_indexes::IndexAuditArgs),

    /// Check for schema drift between migrations and live database.
    ///
    /// Compares the schema produced by replaying all migrations against the live schema,
    /// catching drift caused by manual out-of-band changes.
    SchemaDrift(commands::schema_drift::SchemaDriftArgs),

    /// Analyze connection pool utilization and recommend sizing.
    PoolAdvisor(commands::pool_advisor::PoolAdvisorArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Setup(args) => commands::setup::run(args),
        Commands::Test(args) => commands::test::run(args),
        Commands::Lint(args) => commands::lint::run(args),
        Commands::Release(args) => commands::release::run(args),
        Commands::CompatMatrix(args) => commands::compat::run(args),
        Commands::AuditIndexes(args) => commands::audit_indexes::run(args),
        Commands::SchemaDrift(args) => commands::schema_drift::run(args),
        Commands::PoolAdvisor(args) => commands::pool_advisor::run(args),
    }
}
