use dotenvy::dotenv;
use serde::Deserialize;
use std::env;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_port: u16,
    pub database_url: String,
    pub database_replica_url: Option<String>,
    pub stellar_horizon_url: String,
    pub anchor_webhook_secret: String,
}
impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv().ok(); // Load .env file if present

        Ok(Config {
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            database_url: env::var("DATABASE_URL")?,
            database_replica_url: env::var("DATABASE_REPLICA_URL").ok(),
            stellar_horizon_url: env::var("STELLAR_HORIZON_URL")?,
            anchor_webhook_secret: env::var("ANCHOR_WEBHOOK_SECRET")?,
        })
    }
}
