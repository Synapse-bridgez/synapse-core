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
        Commands::Init => run_init().await,
        Commands::Completions { shell } => print_completions(&shell),
        Commands::External(args) => run_external(&args, base_url, api_key),
    };

    if let Err(e) = result {
        std::process::exit(synapse_cli::handle_anyhow_error(e));
    }

    Ok(())
}

/// Invoke an external `synapse-cli-<name>` plugin found on `PATH`.
///
/// Follows the `git`/`cargo` external-subcommand convention: the first
/// argument selects the plugin binary, and all remaining arguments are passed
/// through verbatim. The resolved auth/config context is exported via
/// environment variables so plugins do not need to re-implement the CLI's own
/// configuration resolution.
fn run_external(args: &[String], base_url: &str, api_key: &str) -> Result<()> {
    use std::process::Command;

    let name = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("no external subcommand specified"))?;
    let binary = format!("synapse-cli-{name}");

    let status = Command::new(&binary)
        .args(&args[1..])
        .env("SYNAPSE_BASE_URL", base_url)
        .env("SYNAPSE_API_KEY", api_key)
        .env("SYNAPSE_PLUGIN", name)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "unknown subcommand '{name}': no '{binary}' found on PATH"
                )
            } else {
                anyhow::anyhow!("failed to run plugin '{binary}': {e}")
            }
        })?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// Interactive first-time setup wizard.
///
/// Prompts for the server URL, API key, and default output format, validates
/// connectivity with a lightweight health check, and writes the result to the
/// same configuration file the CLI already reads from. Re-running when a
/// config exists requires explicit confirmation before overwriting.
async fn run_init() -> Result<()> {
    use std::io::{self, Write};

    let config_path = synapse_cli::config::config_file_path()?;

    if config_path.exists() {
        print!(
            "A configuration already exists at {}. Overwrite it? [y/N] ",
            config_path.display()
        );
        io::stdout().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("Aborted; existing configuration left unchanged.");
            return Ok(());
        }
    }

    let base_url = prompt("Server URL", "http://localhost:8080")?;
    let base_url = base_url.trim().trim_end_matches('/').to_string();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        anyhow::bail!("Server URL must start with http:// or https://");
    }

    let api_key = prompt("API key", "")?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        anyhow::bail!("API key must not be empty");
    }

    let output_format = prompt("Default output format (json|table)", "json")?;
    let output_format = output_format.trim().to_ascii_lowercase();
    if !matches!(output_format.as_str(), "json" | "table") {
        anyhow::bail!("Output format must be 'json' or 'table'");
    }

    println!("Validating credentials against {base_url} ...");
    let client = synapse_cli::client::Client::new(base_url.clone(), api_key.clone());
    client.health_check().await.map_err(|e| {
        anyhow::anyhow!(
            "Could not validate credentials against {base_url}: {e}. Configuration was not saved."
        )
    })?;

    synapse_cli::config::save_config(&synapse_cli::config::Config {
        base_url,
        api_key,
        output_format,
    })?;

    println!("Configuration saved to {}", config_path.display());
    Ok(())
}

fn prompt(label: &str, default: &str) -> Result<String> {
    use std::io::{self, Write};

    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(input.to_string())
    }
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
