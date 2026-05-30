//! # Database Module
//!
//! Provides secure, tested connection pooling and error handling for PostgreSQL operations.
//!
//! ## Pool Creation
//!
//! Two pool builders are provided:
//! - [`create_pool`]: Default pool for general queries (enforces statement timeout)
//! - [`create_long_running_pool`]: Separate pool for background jobs with extended timeouts
//!
//! Both eagerly warm up connections and set per-connection statement timeouts.
//!
//! ## Error Handling
//!
//! Connection errors are returned as `sqlx::Error`. Callers must:
//! - Handle `PoolTimedOut` errors (see [`queries::with_timeout`])
//! - Log connection failures for alerting
//! - Retry transient errors with backoff
//!
//! ## Configuration
//!
//! Database settings are loaded from [`config::Config`]:
//! - `db_min_connections`: Minimum pool size (default 10)
//! - `db_max_connections`: Maximum pool size (default 50)
//! - `db_idle_timeout_secs`: Close idle connections after N seconds
//! - `db_statement_timeout_ms`: Per-statement timeout (default 30s)
//! - `db_long_running_statement_timeout_ms`: Separate timeout for background tasks
//!
//! ## Security
//!
//! - All connections use SSL/TLS if configured in `database_url`
//! - Connection strings never logged; only error details logged
//! - RLS policies enforced via tenant context (see [`queries::set_tenant_context`])

use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub mod audit;
pub mod cron;
pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;
pub mod session;
pub mod slow_query;

/// Build a pool and eagerly establish `min_connections` by running `SELECT 1`
/// on each connection before returning. Logs warm-up completion time.
///
/// # Error Handling
///
/// Returns `sqlx::Error` if:
/// - Connection URL is invalid or unreachable
/// - Connection limit exceeded (rare; indicates pool saturation)
/// - Database refuses connection (auth, SSL, etc.)
///
/// # Configuration
///
/// Pool sizing from config:
/// - `db_min_connections`: Connections to warm up at startup
/// - `db_max_connections`: Hard limit on concurrent connections
/// - `db_idle_timeout_secs`: Auto-close idle connections
/// - `db_statement_timeout_ms`: Per-query timeout on all connections
///
/// # Performance Optimization
///
/// - Min connections are pre-warmed to reduce query latency
/// - Statement timeout prevents slow queries from hogging connections
/// - Idle timeout frees resources during low-traffic periods
/// - Max connections limits database resource consumption
///
/// # Examples
/// ```ignore
/// let config = Config::from_env();
/// let pool = create_pool(&config).await?;
/// ```
pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let statement_timeout_ms = config.db_statement_timeout_ms;
    let idle_timeout_secs = config.db_idle_timeout_secs;

    let pool = PgPoolOptions::new()
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
        .await?;
    
    // Warm up connections to reduce initial query latency
    warm_up(&pool, config.db_min_connections).await?;
    
    tracing::info!(
        min_connections = config.db_min_connections,
        max_connections = config.db_max_connections,
        "Connection pool initialized and warmed up"
    );
    
    Ok(pool)
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

/// Pre-warm all minimum connections by executing `SELECT 1` in parallel.
///
/// This reduces latency for initial queries and verifies database connectivity.
/// Failures are logged but don't fail the pool creation (fail-open approach).
///
/// # Performance Impact
///
/// - Adds 100-500ms at startup to establish connections
/// - Eliminates connection acquisition latency for first N queries
/// - Detects database connectivity issues before accepting traffic
async fn warm_up(pool: &PgPool, min_connections: u32) -> Result<(), sqlx::Error> {
    let start = std::time::Instant::now();
    let mut handles = Vec::with_capacity(min_connections as usize);
    
    for i in 0..min_connections {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            match sqlx::query("SELECT 1").execute(&pool).await {
                Ok(_) => {
                    tracing::debug!(connection = i, "Connection warmed up");
                    Ok(())
                }
                Err(e) => {
                    tracing::warn!(connection = i, error = %e, "Failed to warm up connection");
                    Ok(()) // Don't fail on individual connection warmup
                }
            }
        }));
    }
    
    for handle in handles {
        handle.await.ok();
    }
    
    let elapsed = start.elapsed();
    tracing::info!(
        connections = min_connections,
        duration_ms = elapsed.as_millis(),
        "Pool warm-up completed"
    );
    
    Ok(())
}
