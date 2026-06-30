pub mod client;
pub mod error;
pub mod formatter;

pub use client::SynapseCliClient;
pub use error::{
    handle_error, map_http_error, map_network_error, CliError, EXIT_AUTH_FAILURE, EXIT_NOT_FOUND,
    EXIT_OTHER,
};
pub use formatter::{Formatter, OutputFormat};

#[derive(Debug)]
pub struct CliConfig {
    pub base_url: String,
    pub api_key: Option<String>,
}

impl CliConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let base_url =
            std::env::var("SYNAPSE_BASE_URL").unwrap_or_else(|_| "http://localhost:3000".into());
        let api_key = std::env::var("SYNAPSE_API_KEY").ok();

        Ok(Self { base_url, api_key })
    }
}
