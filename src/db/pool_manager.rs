use sqlx::{postgres::PgPoolOptions, PgPool};
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct PoolManager {
    primary: PgPool,
    replica: Option<PgPool>,
    failover_state: Arc<RwLock<FailoverState>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FailoverState {
    primary_healthy: bool,
    replica_healthy: bool,
}

impl PoolManager {
    pub async fn new(
        primary_url: &str,
        replica_url: Option<&str>,
        max_connections: u32,
    ) -> Result<Self, sqlx::Error> {
        let primary = build_pool(primary_url, max_connections).await?;

        let replica = if let Some(url) = replica_url {
            Some(build_pool(url, max_connections).await?)
        } else {
            None
        };

        Ok(Self {
            primary,
            replica,
            failover_state: Arc::new(RwLock::new(FailoverState {
                primary_healthy: true,
                replica_healthy: true,
            })),
        })
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    pub fn replica(&self) -> Option<&PgPool> {
        self.replica.as_ref()
    }

    pub async fn read_pool(&self) -> (&PgPool, bool) {
        let state = self.failover_state.read().await;

        if let Some(replica) = &self.replica {
            if state.replica_healthy {
                tracing::info!("Routing read query to replica database");
                return (replica, true);
            }
        }

        (&self.primary, false)
    }

    pub async fn get_read_pool(&self) -> &PgPool {
        self.read_pool().await.0
    }

    pub async fn get_write_pool(&self) -> &PgPool {
        &self.primary
    }

    /// Spawn a background task that periodically probes the replica (and
    /// primary) with a cheap query and writes the result to `failover_state`.
    ///
    /// Without this, `failover_state` is only ever set once at construction
    /// time and `read_pool()` would keep routing to the replica forever,
    /// even after it becomes unreachable.
    pub fn start_health_checks(&self) {
        if self.replica.is_none() {
            return;
        }

        let replica = self.replica.clone();
        let primary = self.primary.clone();
        let failover_state = Arc::clone(&self.failover_state);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(HEALTH_CHECK_INTERVAL);
            loop {
                ticker.tick().await;

                let replica_healthy = match &replica {
                    Some(pool) => sqlx::query("SELECT 1").execute(pool).await.is_ok(),
                    None => false,
                };
                let primary_healthy = sqlx::query("SELECT 1").execute(&primary).await.is_ok();

                let mut state = failover_state.write().await;
                if state.replica_healthy != replica_healthy {
                    tracing::warn!(
                        replica_healthy,
                        "Replica health changed; updating failover state"
                    );
                }
                if state.primary_healthy != primary_healthy {
                    tracing::warn!(
                        primary_healthy,
                        "Primary health changed; updating failover state"
                    );
                }
                state.replica_healthy = replica_healthy;
                state.primary_healthy = primary_healthy;
            }
        });
    }

    /// Gracefully shutdown all managed pools (primary and optional replica).
    /// Waits for in-flight queries to complete before closing.
    pub async fn graceful_shutdown(&self) {
        tracing::info!("Gracefully shutting down PoolManager pools");

        // Shutdown primary pool
        if !self.primary.is_closed() {
            shutdown_pool(&self.primary, "primary").await;
        }

        // Shutdown replica pool if configured
        if let Some(replica) = &self.replica {
            if !replica.is_closed() {
                shutdown_pool(replica, "replica").await;
            }
        }

        tracing::info!("PoolManager graceful shutdown complete");
    }
}

/// How often the background health-check task probes primary/replica.
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Maximum time to wait for in-flight queries to finish during graceful shutdown.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

fn build_pool(
    url: &str,
    max_connections: u32,
) -> impl std::future::Future<Output = Result<PgPool, sqlx::Error>> + '_ {
    PgPoolOptions::new()
        .max_connections(max_connections)
        // Fail fast instead of hanging when the pool is exhausted.
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
}

/// Gracefully shutdown a single pool, waiting for in-flight queries.
async fn shutdown_pool(pool: &PgPool, name: &str) {
    if pool.is_closed() {
        tracing::debug!("Database {} pool already closed; skipping graceful shutdown", name);
        return;
    }

    let active = pool.size().saturating_sub(pool.num_idle() as u32);
    tracing::info!(
        pool_name = name,
        active_connections = active,
        timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
        "Starting graceful shutdown of {} pool",
        name
    );

    let drained = tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, async {
        loop {
            let in_flight = pool.size().saturating_sub(pool.num_idle() as u32);
            if in_flight == 0 {
                break;
            }
            tracing::debug!(in_flight, "Waiting for {} pool in-flight queries to complete", name);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;

    if drained.is_err() {
        let remaining = pool.size().saturating_sub(pool.num_idle() as u32);
        tracing::warn!(
            pool_name = name,
            remaining_connections = remaining,
            timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
            "Graceful shutdown timeout exceeded for {} pool; forcing close",
            name
        );
    }

    pool.close().await;
    tracing::info!("Database {} pool closed", name);
}
