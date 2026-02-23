use sqlx::postgres::{PgPool, PgPoolOptions};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Error, Debug)]
pub enum PoolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("No healthy replica available")]
    NoHealthyReplica,
    #[error("Primary connection failed")]
    PrimaryFailed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueryType {
    Read,
    Write,
}

pub struct PoolManager {
    primary: Arc<PgPool>,
    replicas: Arc<RwLock<Vec<ReplicaPool>>>,
    health_check_interval: Duration,
}

struct ReplicaPool {
    pool: PgPool,
    healthy: bool,
}

impl PoolManager {
    pub async fn new(
        primary_url: &str,
        replica_urls: Vec<String>,
        max_connections: u32,
    ) -> Result<Self, PoolError> {
        let primary = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(primary_url)
            .await?;

        let mut replicas = Vec::new();
        for url in replica_urls {
            match PgPoolOptions::new()
                .max_connections(max_connections)
                .connect(&url)
                .await
            {
                Ok(pool) => replicas.push(ReplicaPool {
                    pool,
                    healthy: true,
                }),
                Err(e) => {
                    tracing::warn!("Failed to connect to replica {}: {}", url, e);
                }
            }
        }

        Ok(Self {
            primary: Arc::new(primary),
            replicas: Arc::new(RwLock::new(replicas)),
            health_check_interval: Duration::from_secs(30),
        })
    }

    pub async fn execute_query<T, F>(
        &self,
        query_type: QueryType,
        query_fn: F,
    ) -> Result<T, PoolError>
    where
        F: FnOnce(&PgPool) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, sqlx::Error>> + Send>> + Send,
    {
        match query_type {
            QueryType::Write => {
                let result = query_fn(&self.primary).await?;
                Ok(result)
            }
            QueryType::Read => {
                // Try replicas first
                let replicas = self.replicas.read().await;
                for replica in replicas.iter() {
                    if replica.healthy {
                        match query_fn(&replica.pool).await {
                            Ok(result) => return Ok(result),
                            Err(e) => {
                                tracing::warn!("Replica query failed: {}", e);
                                continue;
                            }
                        }
                    }
                }
                drop(replicas);

                // Fallback to primary if all replicas fail
                tracing::info!("All replicas failed, falling back to primary");
                let result = query_fn(&self.primary).await?;
                Ok(result)
            }
        }
    }

    pub async fn check_health(&self) -> Result<HealthStatus, PoolError> {
        let primary_healthy = sqlx::query("SELECT 1")
            .execute(&*self.primary)
            .await
            .is_ok();

        let mut replicas = self.replicas.write().await;
        let mut healthy_replicas = 0;

        for replica in replicas.iter_mut() {
            let is_healthy = sqlx::query("SELECT 1")
                .execute(&replica.pool)
                .await
                .is_ok();
            
            replica.healthy = is_healthy;
            if is_healthy {
                healthy_replicas += 1;
            }
        }

        Ok(HealthStatus {
            primary_healthy,
            healthy_replicas,
            total_replicas: replicas.len(),
        })
    }

    pub fn primary_pool(&self) -> &PgPool {
        &self.primary
    }

    pub async fn get_replica_count(&self) -> usize {
        self.replicas.read().await.len()
    }

    pub async fn get_healthy_replica_count(&self) -> usize {
        self.replicas.read().await.iter().filter(|r| r.healthy).count()
    }
}

#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub primary_healthy: bool,
    pub healthy_replicas: usize,
    pub total_replicas: usize,
}
