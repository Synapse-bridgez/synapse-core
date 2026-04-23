use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod audit;
pub mod cron;
pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;

pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .min_connections(config.db_min_connections)
        .max_connections(config.db_max_connections)
        .connect(&config.database_url)
        .await
}
