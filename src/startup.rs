use crate::config::Config;
use anyhow::{Context, Result};
use sqlx::PgPool;
use std::time::Duration;

pub struct ValidationReport {
    pub environment: bool,
    pub database: bool,
    pub redis: bool,
    pub horizon: bool,
    pub errors: Vec<String>,
}

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        self.environment && self.database && self.redis && self.horizon
    }

    pub fn print(&self) {
        println!("\n=== Startup Validation Report ===");
        println!("Environment Variables: {}", status(self.environment));
        println!("Database Connectivity: {}", status(self.database));
        println!("Redis Connectivity:    {}", status(self.redis));
        println!("Horizon Connectivity:  {}", status(self.horizon));
        
        if !self.errors.is_empty() {
            println!("\nErrors:");
            for error in &self.errors {
                println!("  ❌ {}", error);
            }
        }
        
        println!("\nOverall Status: {}", if self.is_valid() { "✅ PASS" } else { "❌ FAIL" });
        println!("=================================\n");
    }
}

fn status(ok: bool) -> &'static str {
    if ok { "✅ OK" } else { "❌ FAIL" }
}

pub async fn validate_environment(config: &Config, pool: &PgPool) -> Result<ValidationReport> {
    let mut report = ValidationReport {
        environment: true,
        database: true,
        redis: true,
        horizon: true,
        errors: Vec::new(),
    };

    // Validate environment variables
    if let Err(e) = validate_env_vars(config) {
        report.environment = false;
        report.errors.push(format!("Environment: {}", e));
    }

    // Validate database
    if let Err(e) = validate_database(pool).await {
        report.database = false;
        report.errors.push(format!("Database: {}", e));
    }

    // Validate Redis
    if let Err(e) = validate_redis(&config.redis_url).await {
        report.redis = false;
        report.errors.push(format!("Redis: {}", e));
    }

    // Validate Horizon
    if let Err(e) = validate_horizon(&config.stellar_horizon_url).await {
        report.horizon = false;
        report.errors.push(format!("Horizon: {}", e));
    }

    Ok(report)
}

fn validate_env_vars(config: &Config) -> Result<()> {
    if config.database_url.is_empty() {
        anyhow::bail!("DATABASE_URL is empty");
    }
    if config.stellar_horizon_url.is_empty() {
        anyhow::bail!("STELLAR_HORIZON_URL is empty");
    }
    if config.redis_url.is_empty() {
        anyhow::bail!("REDIS_URL is empty");
    }
    if config.server_port == 0 {
        anyhow::bail!("SERVER_PORT must be greater than 0");
    }
    
    // Validate URL formats
    url::Url::parse(&config.stellar_horizon_url)
        .context("STELLAR_HORIZON_URL is not a valid URL")?;
    
    Ok(())
}

async fn validate_database(pool: &PgPool) -> Result<()> {
    sqlx::query("SELECT 1")
        .fetch_one(pool)
        .await
        .context("Failed to connect to database")?;
    
    // Check if migrations are up to date
    let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
        .context("Failed to check migrations table")?;
    
    if applied == 0 {
        anyhow::bail!("No migrations applied");
    }
    
    Ok(())
}

async fn validate_redis(redis_url: &str) -> Result<()> {
    let client = redis::Client::open(redis_url)
        .context("Invalid Redis URL")?;
    
    let mut conn = client
        .get_multiplexed_tokio_connection()
        .await
        .context("Failed to connect to Redis")?;
    
    redis::cmd("PING")
        .query_async::<_, String>(&mut conn)
        .await
        .context("Redis PING failed")?;
    
    Ok(())
}

async fn validate_horizon(horizon_url: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    
    let response = client
        .get(horizon_url)
        .send()
        .await
        .context("Failed to connect to Horizon")?;
    
    if !response.status().is_success() {
        anyhow::bail!("Horizon returned status: {}", response.status());
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_env_vars_empty_database_url() {
        let config = Config {
            server_port: 3000,
            database_url: String::new(),
            stellar_horizon_url: "https://horizon-testnet.stellar.org".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            cors_allowed_origins: None,
            log_request_body: false,
        };
        
        assert!(validate_env_vars(&config).is_err());
    }

    #[test]
    fn test_validate_env_vars_invalid_url() {
        let config = Config {
            server_port: 3000,
            database_url: "postgres://localhost:5432/test".to_string(),
            stellar_horizon_url: "not-a-url".to_string(),
            redis_url: "redis://localhost:6379".to_string(),
            cors_allowed_origins: None,
            log_request_body: false,
        };
        
        assert!(validate_env_vars(&config).is_err());
    }
}
