use sqlx::postgres::{PgPool, PgPoolOptions};
use crate::config::Config;

pub mod models;
pub mod queries;

pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await
}
