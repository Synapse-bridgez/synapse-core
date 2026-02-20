use sqlx::postgres::{PgPool, PgPoolOptions};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

#[derive(Clone)]
pub struct PoolManager {
    primary: Arc<PgPool>,
    replica: Option<Arc<PgPool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    Read,
    Write,
}

impl PoolManager {
    pub async fn new(
        primary_url: &str,
        replica_url: Option<&str>,
    ) -> Result<Self, sqlx::Error> {
        let primary = Arc::new(
            PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(Duration::from_secs(5))
                .after_connect(|conn, _meta| {
                    Box::pin(async move {
                        Self::reconnect_with_backoff(conn).await;
                        Ok(())
                    })
                })
                .connect(primary_url)
                .await?,
        );

        let replica = if let Some(url) = replica_url {
            Some(Arc::new(
                PgPoolOptions::new()
                    .max_connections(10)
                    .acquire_timeout(Duration::from_secs(5))
                    .after_connect(|conn, _meta| {
                        Box::pin(async move {
                            Self::reconnect_with_backoff(conn).await;
                            Ok(())
                        })
                    })
                    .connect(url)
                    .await?,
            ))
        } else {
            None
        };

        Ok(Self { primary, replica })
    }

    pub fn get_pool(&self, intent: QueryIntent) -> &PgPool {
        match intent {
            QueryIntent::Write => &self.primary,
            QueryIntent::Read => self.replica.as_ref().map(|r| r.as_ref()).unwrap_or(&self.primary),
        }
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    pub fn replica(&self) -> Option<&PgPool> {
        self.replica.as_ref().map(|r| r.as_ref())
    }

    pub async fn health_check(&self) -> HealthCheckResult {
        let primary_healthy = sqlx::query("SELECT 1")
            .execute(self.primary.as_ref())
            .await
            .is_ok();

        let replica_healthy = if let Some(replica) = &self.replica {
            sqlx::query("SELECT 1")
                .execute(replica.as_ref())
                .await
                .is_ok()
        } else {
            true // No replica configured, consider healthy
        };

        HealthCheckResult {
            primary: primary_healthy,
            replica: replica_healthy,
        }
    }

    async fn reconnect_with_backoff(conn: &mut sqlx::pool::PoolConnection<sqlx::Postgres>) {
        let mut attempt = 0;
        let max_attempts = 5;

        while attempt < max_attempts {
            match sqlx::query("SELECT 1").execute(&mut **conn).await {
                Ok(_) => {
                    tracing::info!("Database connection established");
                    return;
                }
                Err(e) => {
                    attempt += 1;
                    let backoff = Duration::from_secs(2u64.pow(attempt));
                    tracing::warn!(
                        "Connection attempt {} failed: {}. Retrying in {:?}",
                        attempt,
                        e,
                        backoff
                    );
                    sleep(backoff).await;
                }
            }
        }
        tracing::error!("Failed to establish connection after {} attempts", max_attempts);
    }
}

#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    pub primary: bool,
    pub replica: bool,
}
