use anyhow::Result;
use dotenvy::dotenv;
use std::env;
use ipnet::IpNet;
use crate::secrets::SecretsManager;

#[derive(Debug, Clone)]
pub enum AllowedIps {
    Any,
    Cidrs(Vec<IpNet>),
}

#[derive(Debug, Clone)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub server_port: u16,
    pub database_url: String,
    pub database_replica_url: Option<String>,
    pub stellar_horizon_url: String,
    pub anchor_webhook_secret: String,
    pub redis_url: String,
    pub default_rate_limit: u32,
    pub whitelist_rate_limit: u32,
    pub whitelisted_ips: String,
    pub log_format: LogFormat,
    pub allowed_ips: AllowedIps,
    pub backup_dir: String,
    pub backup_encryption_key: Option<String>,
}

pub mod assets;
impl Config {
    pub async fn load() -> anyhow::Result<Self> {
        dotenv().ok(); // Load .env file if present

        let allowed_ips =
            parse_allowed_ips(&env::var("ALLOWED_IPS").unwrap_or_else(|_| "*".to_string()))?;

        let log_format = parse_log_format(
            &env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string()),
        )?;

        let use_vault = env::var("VAULT_ROLE_ID").is_ok() && env::var("VAULT_SECRET_ID").is_ok();

        let (database_url, anchor_webhook_secret) = if use_vault {
            let secrets = SecretsManager::new().await?;
            let db_password = secrets.get_db_password().await?;
            let anchor_secret = secrets.get_anchor_secret().await?;

            let db_template = env::var("DATABASE_URL_TEMPLATE").ok();
            let db_url = db_template
                .map(|template| template.replace("{password}", &db_password))
                .unwrap_or_else(|| env::var("DATABASE_URL").unwrap_or_default());

            (db_url, anchor_secret)
        } else {
            (
                env::var("DATABASE_URL")?,
                env::var("ANCHOR_WEBHOOK_SECRET")?,
            )
        };

        Ok(Config {
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            database_url,
            database_replica_url: env::var("DATABASE_REPLICA_URL").ok(),
            stellar_horizon_url: env::var("STELLAR_HORIZON_URL")?,
            anchor_webhook_secret: anchor_webhook_secret,
            redis_url: env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            default_rate_limit: env::var("DEFAULT_RATE_LIMIT")
                .unwrap_or_else(|_| "100".to_string())
                .parse()?,
            whitelist_rate_limit: env::var("WHITELIST_RATE_LIMIT")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()?,
            whitelisted_ips: env::var("WHITELISTED_IPS").unwrap_or_default(),
            log_format,
            allowed_ips,
            backup_dir: env::var("BACKUP_DIR").unwrap_or_else(|_| "./backups".to_string()),
            backup_encryption_key: env::var("BACKUP_ENCRYPTION_KEY").ok(),
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

#[derive(Clone, Debug)]
pub enum AllowedIps {
    Any,
    Cidrs(Vec<ipnet::IpNet>),
}
