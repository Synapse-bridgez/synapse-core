mod client;
mod formatter;

use clap::{Parser, Subcommand};
use client::{ClientError, SynapseApiClient};
use formatter::Formatter;

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI for interacting with the Synapse API", long_about = None)]
struct Cli {
    #[arg(long, env = "SYNAPSE_BASE_URL", default_value = "http://localhost:8080")]
    base_url: String,

    #[arg(long, env = "SYNAPSE_API_KEY", default_value = "")]
    api_key: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage transactions
    Transactions {
        #[command(subcommand)]
        command: TransactionCommand,
    },
}

#[derive(Subcommand)]
enum TransactionCommand {
    #[command(about = "Fetch a single transaction by its UUID",
              long_about = "Fetch a single transaction by its UUID.\n\n\
                            Exit codes:\n  \
                            0 - Success\n  \
                            1 - Transaction not found or other error\n\n\
                            Output formats:\n  \
                            table - Human-readable table (default)\n  \
                            json - Pretty-printed JSON\n\n\
                            Not-found errors (HTTP 404) are surfaced as exit code 1 with message \
                            'transaction not found: <error message>', distinguishing them from other failure modes.")]
    Get {
        /// Transaction ID (UUID)
        id: String,
        /// Output format: 'table' (default) or 'json'
        #[arg(long, default_value = "table")]
        format: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Transactions { command } => match command {
            TransactionCommand::Get { id, format } => {
                let client = SynapseApiClient::new(cli.base_url, cli.api_key);
                match client.get_transaction(&id).await {
                    Ok(tx) => {
                        let output = Formatter::format(&format, &tx);
                        println!("{}", output);
                        std::process::exit(0);
                    }
                    Err(ClientError::NotFound(msg)) => {
                        eprintln!("transaction not found: {}", msg);
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        },
    }
}
