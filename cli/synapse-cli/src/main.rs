use clap::Parser;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

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

fn main() {
    let _args = Args::parse();
    let _config = load_config();
}
