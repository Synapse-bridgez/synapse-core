use clap::{Subcommand, Args};

#[derive(Args)]
pub struct TransactionsCmd {
    #[command(subcommand)]
    pub command: TransactionsSubcommand,
}

#[derive(Subcommand)]
pub enum TransactionsSubcommand {
    /// Export transactions with optional filters
    Export {
        /// Export format (csv or json)
        #[arg(long, default_value = "csv")]
        format: String,

        /// Start date filter (YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,

        /// End date filter (YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,

        /// Filter by transaction status
        #[arg(long)]
        status: Option<String>,

        /// Filter by asset code
        #[arg(long)]
        asset_code: Option<String>,

        /// Output file path (default: stdout)
        #[arg(long)]
        output: Option<String>,
    },
}
