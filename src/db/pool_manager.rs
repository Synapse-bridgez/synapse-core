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

    /// Gracefully drain and close the primary pool and, if configured, the
    /// replica pool. Mirrors [`crate::db::graceful_shutdown`] so that
    /// `PoolManager`'s pools are drained on process shutdown alongside the
    /// application's main pool, instead of being dropped mid-query.
    pub async fn graceful_shutdown(&self) {
        crate::db::graceful_shutdown(&self.primary).await;
        if let Some(replica) = &self.replica {
            crate::db::graceful_shutdown(replica).await;
        }
    }
}

fn build_pool(
    url: &str,
    max_connections: u32,
) -> impl std::future::Future<Output = Result<PgPool, sqlx::Error>> + '_ {
    PgPoolOptions::new()
        .max_connections(max_connections)
        // Fail fast instead of hanging when the pool is exhausted.
        .acquire_timeout(Duration::from_secs(5))
        // Defaults every pooled connection to RLS admin context — see
        // crate::db::set_session_admin_context for why this is required now
        // that the app's role no longer bypasses RLS.
        .after_connect(|conn, _meta| {
            Box::pin(async move { crate::db::set_session_admin_context(conn).await })
        })
        .connect(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for issue #1060 Part B: `PoolManager`'s pools were
    /// never drained on shutdown, only dropped — any in-flight query on them
    /// would be abruptly cut regardless of the main pool's graceful drain.
    #[tokio::test]
    async fn graceful_shutdown_closes_the_primary_pool() {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string()
        });
        let manager = match PoolManager::new(&database_url, None, 2).await {
            Ok(manager) => manager,
            Err(_) => {
                eprintln!(
                    "skipping graceful_shutdown_closes_the_primary_pool: database not reachable"
                );
                return;
            }
        };

        assert!(!manager.primary().is_closed());
        manager.graceful_shutdown().await;
        assert!(
            manager.primary().is_closed(),
            "graceful_shutdown must close the primary pool, not just drop its handle"
        );
    }
}
