use anyhow::Result;
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

pub mod assets;
impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv().ok(); // Load .env file if present

        let _allowed_ips =
            parse_allowed_ips(&env::var("ALLOWED_IPS").unwrap_or_else(|_| "*".to_string()))?;

        let log_format = parse_log_format(
            &env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string()),
        )?;

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

fn parse_allowed_ips(raw: &str) -> anyhow::Result<AllowedIps> {
    let value = raw.trim();
    if value == "*" {
        return Ok(AllowedIps::Any);
    }

    let cidrs = value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::parse::<IpNet>)
        .collect::<Result<Vec<_>, _>>()?;

    if cidrs.is_empty() {
        anyhow::bail!("ALLOWED_IPS must be '*' or a comma-separated list of CIDRs");
    }

    Ok(AllowedIps::Cidrs(cidrs))
}

fn parse_log_format(raw: &str) -> anyhow::Result<LogFormat> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => anyhow::bail!("LOG_FORMAT must be 'text' or 'json'"),
    }
}
