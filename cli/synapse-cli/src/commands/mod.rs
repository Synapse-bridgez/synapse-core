pub mod events;
pub mod health;
pub mod settlements;
pub mod stats;
pub mod transactions;

pub use events::EventsCmd;
pub use settlements::SettlementsCmd;
pub use transactions::TransactionsCmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "synapse",
    version = "0.1.0",
    about = "Synapse API command-line tool",
    long_about = "Interact with a running Synapse server from the command line.\n\n\
                  Set SYNAPSE_BASE_URL and SYNAPSE_API_KEY in your environment, \
                  or pass --base-url / --api-key explicitly."
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
    /// Real-time event streaming commands
    #[command(subcommand)]
    Events(events::EventsSubcommand),

    /// Health and readiness probe commands
    #[command(subcommand)]
    Health(health::HealthCommand),

    /// Transaction statistics commands
    #[command(subcommand)]
    Stats(stats::StatsCommand),
}
