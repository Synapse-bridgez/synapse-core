pub mod commands;
pub mod formatter;
pub mod client;

pub use formatter::{OutputFormat, Formatter};
pub use client::SynapseCliClient;

#[derive(Debug)]
pub struct CliConfig {
    pub base_url: String,
    pub api_key: Option<String>,
}

impl CliConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let base_url = std::env::var("SYNAPSE_URL")
            .unwrap_or_else(|_| "http://localhost:3000".to_string());

        let api_key = std::env::var("SYNAPSE_API_KEY").ok();

        Ok(CliConfig { base_url, api_key })
    }
}
