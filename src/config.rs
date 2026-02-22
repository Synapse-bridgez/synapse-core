use dotenvy::dotenv;
use serde::Deserialize;
use std::env;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_port: u16,
    pub database_url: String,
    pub stellar_horizon_url: String,
    pub redis_url: String,
    /// Comma-separated list of allowed CORS origins (e.g. "http://localhost:3000,https://app.example.com")
    pub cors_allowed_origins: Option<String>,
    /// Enable request body logging (default: false)
    pub log_request_body: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv().ok();

        Ok(Config {
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            database_url: env::var("DATABASE_URL")?,
            stellar_horizon_url: env::var("STELLAR_HORIZON_URL")?,
            redis_url: env::var("REDIS_URL")?,
            cors_allowed_origins: env::var("CORS_ALLOWED_ORIGINS").ok(),
            log_request_body: env::var("LOG_REQUEST_BODY")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),
        })
    }
}

#[derive(Clone, Debug)]
pub enum AllowedIps {
    Any,
    Cidrs(Vec<ipnet::IpNet>),
}
