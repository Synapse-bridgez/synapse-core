use anyhow::Result;
use clap::Parser;
use synapse_cli::commands::{
    events, graphql, health, settlements, stats, transactions, Cli, Commands,
};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let base_url = &cli.base_url;
    let api_key = &cli.api_key;

    let result = match cli.command {
        Commands::Admin(cmd) => synapse_cli::commands::admin::run(cmd, base_url, api_key).await,
        Commands::Events(cmd) => {
            events::handle_events(events::EventsCmd { command: cmd }, base_url).await
        }
        Commands::Health(cmd) => health::run(cmd, base_url, api_key).await,
        Commands::Stats(cmd) => stats::run(cmd, base_url, api_key).await,
        Commands::Settlements(cmd) => settlements::run(cmd.command, base_url, api_key).await,
        Commands::Transactions(cmd) => transactions::run(cmd.command, base_url, api_key).await,
        Commands::Graphql(cmd) => graphql::run(cmd.command, base_url, api_key).await,
        Commands::Completions { shell } => print_completions(&shell),
    };

    if let Err(e) = result {
        let code = match e.downcast::<synapse_cli::CliError>() {
            Ok(cli_err) => synapse_cli::handle_error(cli_err),
            Err(e) => match e.downcast::<synapse_sdk::SynapseError>() {
                Ok(sdk_err) => synapse_cli::handle_error(synapse_cli::from_synapse_error(&sdk_err)),
                Err(e) => {
                    eprintln!("error: {e}");
                    synapse_cli::EXIT_OTHER
                }
            },
        };
        std::process::exit(code);
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
