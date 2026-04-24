use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub mod audit;
pub mod cron;
pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;
pub mod slow_query;

pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let statement_timeout_ms = config.db_statement_timeout_ms;
    let idle_timeout_secs = config.db_idle_timeout_secs;

    PgPoolOptions::new()
        .min_connections(config.db_min_connections)
        .max_connections(config.db_max_connections)
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            let statement_timeout_ms = statement_timeout_ms;
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {}", statement_timeout_ms))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await
}

pub async fn create_long_running_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let statement_timeout_ms = config.db_long_running_statement_timeout_ms;
    let idle_timeout_secs = config.db_idle_timeout_secs;

    PgPoolOptions::new()
        .min_connections(config.db_min_connections)
        .max_connections(config.db_max_connections)
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            let statement_timeout_ms = statement_timeout_ms;
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {}", statement_timeout_ms))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await
}

