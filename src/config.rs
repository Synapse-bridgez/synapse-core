use dotenvy::dotenv;
use ipnet::IpNet;
use serde::Deserialize;
use std::env;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_port: u16,
    pub database_url: String,
    pub stellar_horizon_url: String,
    pub redis_url: String,
}

#[derive(Debug, Clone)]
pub enum AllowedIps {
    Any,
    Cidrs(Vec<IpNet>),
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv().ok(); // Load .env file if present

        let allowed_ips = parse_allowed_ips(
            &env::var("ALLOWED_IPS").unwrap_or_else(|_| "*".to_string()),
        )?;

        Ok(Config {
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            database_url: env::var("DATABASE_URL")?,
            stellar_horizon_url: env::var("STELLAR_HORIZON_URL")?,
            redis_url: env::var("REDIS_URL")?,
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
