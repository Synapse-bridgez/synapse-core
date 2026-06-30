use clap::Parser;
use synapse_cli::commands::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Health(cmd) => {
            synapse_cli::commands::health::run(cmd, &cli.base_url, &cli.api_key).await?;
        }
        Commands::Stats(cmd) => {
            synapse_cli::commands::stats::run(cmd, &cli.base_url, &cli.api_key).await?;
        }
    }

    Ok(())
}
