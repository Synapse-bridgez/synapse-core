pub mod health;
pub mod stats;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "synapse",
    version = "0.1.0",
    about = "Synapse API command-line tool",
    long_about = "Interact with a running Synapse server from the command line.\n\n\
                  Set SYNAPSE_BASE_URL and SYNAPSE_API_KEY in your environment, \
                  or pass --base-url / --api-key explicitly.\n\n\
                  Global flags:\n  \
                  --base-url <URL>    Override the server URL\n  \
                  --api-key  <KEY>    Override the API key",
    arg_required_else_help = true
)]
pub struct Cli {
    /// Base URL of the Synapse server (e.g. http://localhost:3000)
    #[arg(
        long,
        env = "SYNAPSE_BASE_URL",
        default_value = "http://localhost:3000"
    )]
    pub base_url: String,

    /// API key for authentication
    #[arg(long, env = "SYNAPSE_API_KEY", default_value = "")]
    pub api_key: String,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Health and readiness probe commands (live, ready, check, errors)
    #[command(
        about = "Health and readiness probe commands",
        long_about = "Liveness, readiness, and dependency-health probes.\n\n\
                      Subcommands:\n  \
                      live    — Is the process alive?\n  \
                      ready   — Is the server ready to accept traffic?\n  \
                      check   — Full health check including DB and pool stats\n  \
                      errors  — List all registered error codes"
    )]
    Health(health::HealthCommand),

    /// Transaction statistics commands (status, daily, assets, cache)
    #[command(
        about = "Transaction statistics commands",
        long_about = "Query live transaction statistics from the Synapse API.\n\n\
                      Subcommands:\n  \
                      status  — Transaction counts by status\n  \
                      daily   — Daily totals for the last N days\n  \
                      assets  — Totals grouped by asset code\n  \
                      cache   — Query and idempotency cache hit/miss metrics"
    )]
    Stats(stats::StatsCommand),
}
