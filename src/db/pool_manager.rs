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
}

/// How often the background health-check task probes primary/replica.
const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(10);

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
