use synapse_cli::commands::{Cli, Commands};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("synapse_cli=info".parse()?),
        )
        .init();

    let cli = Cli::parse();
    let base_url = &cli.base_url;
    let api_key = &cli.api_key;

    match cli.command {
        Commands::Health(cmd) => {
            synapse_cli::commands::health::run(cmd, base_url, api_key).await?;
        }
        Commands::Stats(cmd) => {
            synapse_cli::commands::stats::run(cmd, base_url, api_key).await?;
        }
    }

    Ok(())
}
