use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::{Duration, Instant};

pub mod audit;
pub mod cron;
pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;
pub mod slow_query;

/// Build a pool and eagerly establish `min_connections` by running `SELECT 1`
/// on each connection before returning. Logs warm-up completion time.
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
                sqlx::query(&format!("SET statement_timeout = {statement_timeout_ms}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await
}

pub async fn create_long_running_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let pool = build_pool(
        &config.database_url,
        config.db_min_connections,
        config.db_max_connections,
        config.db_idle_timeout_secs,
        config.db_long_running_statement_timeout_ms,
    )
    .await?;
    warm_up(&pool, config.db_min_connections).await?;
    Ok(pool)
}

async fn build_pool(
    url: &str,
    min: u32,
    max: u32,
    idle_timeout_secs: u64,
    statement_timeout_ms: u64,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .min_connections(min)
        .max_connections(max)
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {statement_timeout_ms}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
}
