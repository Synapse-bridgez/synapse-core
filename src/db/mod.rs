use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;
pub mod cron;

pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
}
