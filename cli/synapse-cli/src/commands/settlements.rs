use clap::{Subcommand, Args};
use uuid::Uuid;

#[derive(Args)]
pub struct SettlementsCmd {
    #[command(subcommand)]
    pub command: SettlementsSubcommand,
}

#[derive(Subcommand)]
pub enum SettlementsSubcommand {
    /// List settlements with cursor-based pagination
    List {
        /// Pagination cursor
        #[arg(long)]
        cursor: Option<String>,

        /// Page size (1-100, default 10)
        #[arg(long, default_value = "10")]
        limit: i64,

        /// Pagination direction (forward or backward, default forward)
        #[arg(long, default_value = "forward")]
        direction: String,

        /// Output format (table or json)
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Get a specific settlement by ID
    Get {
        /// Settlement UUID
        settlement_id: Uuid,

        /// Output format (table or json)
        #[arg(long, default_value = "table")]
        format: String,
    },
}
