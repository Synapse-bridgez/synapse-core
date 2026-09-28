use anyhow::Result;
use clap::Parser;
use synapse_cli::commands::{
    events, graphql, health, settlements, stats, transactions, Cli, Commands,
};
use synapse_cli::profile::{self, ProfileStore};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Resolve the active profile. An explicit `--profile` override always wins;
    // otherwise fall back to the stored default profile. We never silently pick
    // a different profile than the one the user asked for.
    let store = ProfileStore::load()?;
    let active = match cli.profile.as_deref() {
        Some(name) => store
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown profile: {name}"))?,
        None => store
            .default_profile()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no active profile; run `synapse-cli profile add <name>`"))?,
    };

    // `--base-url` / `--api-key` still override the profile values when given.
    let base_url = cli.base_url.clone().unwrap_or_else(|| active.base_url.clone());
    let api_key = cli.api_key.clone().unwrap_or_else(|| active.api_key.clone());

    // Safety echo: mutating commands must visibly announce the target
    // environment so a destructive command can't silently hit the wrong one.
    if is_mutating(&cli.command) {
        eprintln!(
            "[profile: {}] environment: {} -> {}",
            active.name, active.environment, base_url
        );
    }

    let result = match cli.command {
        Commands::Admin(cmd) => synapse_cli::commands::admin::run(cmd, &base_url, &api_key).await,
        Commands::Events(cmd) => {
            events::handle_events(events::EventsCmd { command: cmd }, &base_url).await
        }
        Commands::Health(cmd) => health::run(cmd, &base_url, &api_key).await,
        Commands::Stats(cmd) => stats::run(cmd, &base_url, &api_key).await,
        Commands::Settlements(cmd) => settlements::run(cmd.command, &base_url, &api_key).await,
        Commands::Transactions(cmd) => transactions::run(cmd.command, &base_url, &api_key).await,
        Commands::Graphql(cmd) => graphql::run(cmd.command, &base_url, &api_key).await,
        Commands::Profile(cmd) => profile::run(cmd, &mut ProfileStore::load()?),
        Commands::Completions { shell } => print_completions(&shell),
    };

    if let Err(e) = result {
        std::process::exit(synapse_cli::handle_anyhow_error(e));
    }

    Ok(())
}

/// Commands that change remote state and therefore warrant the safety echo.
fn is_mutating(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Admin(_) | Commands::Settlements(_) | Commands::Transactions(_)
    )
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
