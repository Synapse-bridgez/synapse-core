use anyhow::Result;
use clap::Parser;
use synapse_cli::commands::{events, graphql, health, settlements, stats, transactions, Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let base_url = &cli.base_url;
    let api_key = &cli.api_key;

    let result = match cli.command {
        Commands::Admin(cmd) => synapse_cli::commands::admin::run(cmd, base_url, api_key).await,
        Commands::Events(cmd) => events::handle_events(events::EventsCmd { command: cmd }, base_url).await,
        Commands::Health(cmd) => health::run(cmd, base_url, api_key).await,
        Commands::Stats(cmd) => stats::run(cmd, base_url, api_key).await,
        Commands::Settlements(cmd) => settlements::run(cmd.command, base_url, api_key).await,
        Commands::Transactions(cmd) => transactions::run(cmd.command, base_url, api_key).await,
        Commands::Graphql(cmd) => graphql::run(cmd.command, base_url).await,
        Commands::Completions { shell } => print_completions(&shell),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }

    Ok(())
}

fn print_completions(shell: &str) -> Result<()> {
    match shell {
        "bash" => println!("_synapse() {{\n    :\n}}\ncomplete -F _synapse synapse"),
        "zsh" => println!("#compdef synapse\ncompdef _synapse synapse\n_synapse() {{\n    :\n}}"),
        "fish" => println!("complete -c synapse -f"),
        _ => anyhow::bail!("Unsupported shell: {shell}"),
    }

    Ok(())
}
