use crate::db::chaos::{ChaosConfig, FaultInjector};
use crate::db::session::{DbSession, InvariantViolation};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct PoolManager {
    primary: PgPool,
    replica: Option<PgPool>,
    failover_state: Arc<RwLock<FailoverState>>,
    injector: Option<FaultInjector>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct FailoverState {
    primary_healthy: bool,
    replica_healthy: bool,
}

impl PoolManager {
    pub async fn new(primary_url: &str, replica_url: Option<&str>) -> Result<Self, sqlx::Error> {
        let primary = PgPoolOptions::new()
            .max_connections(10)
            .connect(primary_url)
            .await?;

        let replica = if let Some(url) = replica_url {
            Some(
                PgPoolOptions::new()
                    .max_connections(10)
                    .connect(url)
                    .await?,
            )
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
            injector: None,
        })
    }

    /// Create PoolManager attached to a chaos fault injector.
    pub async fn with_chaos(
        primary_url: &str,
        replica_url: Option<&str>,
        chaos_config: ChaosConfig,
    ) -> Result<Self, sqlx::Error> {
        let mut manager = Self::new(primary_url, replica_url).await?;
        manager.injector = Some(FaultInjector::new(chaos_config));
        Ok(manager)
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    pub fn replica(&self) -> Option<&PgPool> {
        self.replica.as_ref()
    }

    pub fn injector(&self) -> Option<&FaultInjector> {
        self.injector.as_ref()
    }

    pub async fn get_read_pool(&self) -> &PgPool {
        if let Some(injector) = &self.injector {
            if let Some(fault) = injector.evaluate_fault() {
                if fault == crate::db::chaos::FaultKind::ConnectionDrop {
                    if let Some(replica) = &self.replica {
                        let mut state = self.failover_state.write().await;
                        state.replica_healthy = false;
                        tracing::warn!("Chaos: Replica failure injected, falling back to primary");
                        return &self.primary;
                    }
                }
            }
        }

        let state = self.failover_state.read().await;

        if let Some(replica) = &self.replica {
            if state.replica_healthy {
                return replica;
            }
        }

        &self.primary
    }

    pub async fn get_write_pool(&self) -> &PgPool {
        &self.primary
    }

    /// Create a scoped session with fault injection for invariant tracking.
    pub fn create_session(&self) -> DbSession {
        if let Some(injector) = &self.injector {
            DbSession::with_injector(self.primary.clone(), injector.clone())
        } else {
            DbSession::new(self.primary.clone())
        }
    }

    /// Assert all data consistency invariants across primary pool.
    pub async fn assert_invariants(&self) -> Result<Vec<InvariantViolation>, sqlx::Error> {
        let session = self.create_session();
        session.assert_data_invariants().await
    }

    /// Clean up any orphaned locks or stale connections across pools.
    pub async fn cleanup(&self) -> Result<(), sqlx::Error> {
        let mut session = self.create_session();
        session.cleanup_orphaned_locks().await
    }
}
