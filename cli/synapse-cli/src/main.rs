use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use synapse_sdk::client::SynapseClient;

mod output;

#[derive(Serialize, Deserialize, Debug, Default)]
struct Config {
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI", version)]
struct Args {
    /// API base URL
    #[arg(long, env = "SYNAPSE_BASE_URL")]
    base_url: Option<String>,

    /// API key
    #[arg(long, env = "SYNAPSE_API_KEY")]
    api_key: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Health check subcommands
    Health {
        #[command(subcommand)]
        subcommand: HealthCommand,
    },
}

#[derive(Subcommand)]
enum HealthCommand {
    /// Check if the service is live
    Live,
    /// Check if the service is ready
    Ready,
    /// General health check
    Check,
    /// Get health errors
    Errors,
}

fn load_config() -> Config {
    let config_path = match directories::ProjectDirs::from("", "", "synapse-cli") {
        Some(dirs) => dirs.config_dir().join("config.toml"),
        None => return Config::default(),
    };

    if !config_path.exists() {
        return Config::default();
    }

    match fs::read_to_string(&config_path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(config) => config,
            Err(_) => Config::default(),
        },
        Err(_) => Config::default(),
    }
}

impl Args {
    fn resolve_base_url(&self, config: &Config) -> Option<&str> {
        self.base_url
            .as_deref()
            .or_else(|| config.base_url.as_deref())
    }

    fn resolve_api_key(&self, config: &Config) -> Option<&str> {
        self.api_key
            .as_deref()
            .or_else(|| config.api_key.as_deref())
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = load_config();

    let base_url = match args.resolve_base_url(&config) {
        Some(url) => url,
        None => {
            if args.command.is_some() {
                eprintln!("Error: base_url is required");
                std::process::exit(1);
            }
            return;
        }
    };

    let api_key = match args.resolve_api_key(&config) {
        Some(key) => key,
        None => {
            if args.command.is_some() {
                eprintln!("Error: api_key is required");
                std::process::exit(1);
            }
            return;
        }
    };

    if let Some(Command::Health { subcommand }) = args.command {
        let client = SynapseClient::builder(base_url, api_key).build();
        match subcommand {
            HealthCommand::Live => {
                match client.health().live().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            HealthCommand::Ready => {
                match client.health().ready().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        output::format_output(
                            serde_json::json!({ "error": e.to_string() }),
                            args.json,
                        );
                    }
                }
            }
            HealthCommand::Check => {
                match client.health().health().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            HealthCommand::Errors => {
                match client.health().errors().await {
                    Ok(errors) => output::format_output(errors, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
