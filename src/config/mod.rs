pub mod profiles;

use dotenvy::dotenv;
use serde::Deserialize;
use std::env;
use profiles::{Profile, ProfileDefaults};

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server_port: u16,
    pub database_url: String,
    pub stellar_horizon_url: String,
    pub redis_url: String,
    pub cors_allowed_origins: Option<String>,
}

pub struct ConfigInfo {
    pub config: Config,
    pub profile: Profile,
    pub overrides: Vec<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<ConfigInfo> {
        dotenv().ok();

        let profile = Profile::from_env();
        let defaults = ProfileDefaults::for_profile(profile);
        let mut overrides = Vec::new();

        let server_port = env::var("SERVER_PORT")
            .ok()
            .and_then(|v| {
                overrides.push("SERVER_PORT".to_string());
                v.parse().ok()
            })
            .unwrap_or(defaults.server_port);

        let database_url = env::var("DATABASE_URL").or_else(|_| {
            defaults.database_url.ok_or_else(|| {
                anyhow::anyhow!("DATABASE_URL must be set")
            })
        })?;
        if env::var("DATABASE_URL").is_ok() {
            overrides.push("DATABASE_URL".to_string());
        }

        let stellar_horizon_url = env::var("STELLAR_HORIZON_URL")
            .ok()
            .map(|v| {
                overrides.push("STELLAR_HORIZON_URL".to_string());
                v
            })
            .unwrap_or(defaults.stellar_horizon_url);

        let redis_url = env::var("REDIS_URL")
            .ok()
            .map(|v| {
                overrides.push("REDIS_URL".to_string());
                v
            })
            .unwrap_or(defaults.redis_url);

        let cors_allowed_origins = env::var("CORS_ALLOWED_ORIGINS")
            .ok()
            .map(|v| {
                overrides.push("CORS_ALLOWED_ORIGINS".to_string());
                Some(v)
            })
            .unwrap_or(defaults.cors_allowed_origins);

        Ok(ConfigInfo {
            config: Config {
                server_port,
                database_url,
                stellar_horizon_url,
                redis_url,
                cors_allowed_origins,
            },
            profile,
            overrides,
        })
    }
}
