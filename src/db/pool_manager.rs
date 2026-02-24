use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct PoolManager {
    primary: PgPool,
    replica: Option<PgPool>,
    failover_state: Arc<RwLock<FailoverState>>,
}

#[derive(Debug, Clone)]
struct FailoverState {
    primary_healthy: bool,
    replica_healthy: bool,
}

impl PoolManager {
    pub async fn new(
        primary_url: &str,
        replica_url: Option<&str>,
    ) -> Result<Self, sqlx::Error> {
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
        })
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    pub fn replica(&self) -> Option<&PgPool> {
        self.replica.as_ref()
    }

    pub async fn get_read_pool(&self) -> &PgPool {
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
}