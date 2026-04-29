use crate::services::query_cache::QueryCache;
use sqlx::PgPool;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

/// Partition manager that runs maintenance tasks periodically
pub struct PartitionManager {
    pool: PgPool,
    interval: Duration,
    cache: Option<QueryCache>,
}

impl PartitionManager {
    pub fn new(pool: PgPool, interval_hours: u64, cache: Option<QueryCache>) -> Self {
        Self {
            pool,
            interval: Duration::from_secs(interval_hours * 3600),
            cache,
        }
    }

    /// Start the partition maintenance background task
    pub fn start(self) {
        tokio::spawn(async move {
            let mut interval = time::interval(self.interval);
            interval.tick().await; // Skip first immediate tick

            loop {
                interval.tick().await;
                if let Err(e) = self.maintain_partitions().await {
                    error!("Partition maintenance failed: {}", e);
                } else {
                    info!("Partition maintenance completed successfully");
                }
            }
        });
    }

    /// Run partition maintenance (create new partitions, detach old ones)
    async fn maintain_partitions(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT maintain_partitions()")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Manually trigger partition creation.
    ///
    /// Returns `true` if a new partition was created, `false` if it already existed.
    /// Triggers cache warming when a new partition is created.
    pub async fn create_partition(&self) -> Result<bool, sqlx::Error> {
        // Determine the name of the partition that would be created for next month + 1.
        let partition_name: String = sqlx::query_scalar(
            "SELECT 'transactions_y' || TO_CHAR(DATE_TRUNC('month', NOW() + INTERVAL '2 months'), 'YYYY') \
             || 'm' || TO_CHAR(DATE_TRUNC('month', NOW() + INTERVAL '2 months'), 'MM')",
        )
        .fetch_one(&self.pool)
        .await?;

        let already_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_class WHERE relname = $1)")
                .bind(&partition_name)
                .fetch_one(&self.pool)
                .await?;

        sqlx::query("SELECT create_monthly_partition()")
            .execute(&self.pool)
            .await?;

        let created = !already_exists;
        if created {
            info!(partition = %partition_name, "new partition created, warming cache");
            if let Some(cache) = &self.cache {
                if let Err(e) = cache.warm_cache(&self.pool, None).await {
                    error!("cache warming after partition creation failed: {}", e);
                }
            }
        }
        Ok(created)
    }

    /// Manually trigger old partition detachment
    pub async fn detach_old_partitions(&self, retention_months: i32) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT detach_old_partitions($1)")
            .bind(retention_months)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_partition_manager_creation() {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string()
        });

        let pool = PgPool::connect(&database_url).await.unwrap();
        let manager = PartitionManager::new(pool, 24, None);

        assert_eq!(manager.interval, Duration::from_secs(24 * 3600));
    }

    /// Cache is warm after partition creation; no extra warming if partition already exists.
    #[tokio::test]
    #[ignore]
    async fn test_cache_warm_after_partition_creation() {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string()
        });
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let pool = PgPool::connect(&database_url).await.unwrap();
        let cache = QueryCache::new(&redis_url).expect("redis must be available");

        // Pre-clear any existing warm keys so we can detect a fresh write.
        cache.invalidate("query:status_counts", None).await.ok();
        cache.invalidate("query:daily_totals", None).await.ok();
        cache.invalidate("query:asset_stats", None).await.ok();

        let manager = PartitionManager::new(pool.clone(), 24, Some(cache.clone()));

        // First call: may or may not create a new partition depending on state.
        let created = manager.create_partition().await.unwrap();

        if created {
            // Cache should now be warm.
            let mut conn = cache.client.get_async_connection().await.unwrap();
            let status: Option<Vec<u8>> = conn.get("query:status_counts").await.unwrap();
            let daily: Option<Vec<u8>> = conn.get("query:daily_totals").await.unwrap();
            let assets: Option<Vec<u8>> = conn.get("query:asset_stats").await.unwrap();
            assert!(status.is_some(), "status_counts should be cached");
            assert!(daily.is_some(), "daily_totals should be cached");
            assert!(assets.is_some(), "asset_stats should be cached");
        }

        // Second call: partition already exists → no warming triggered (idempotent).
        let created_again = manager.create_partition().await.unwrap();
        assert!(
            !created_again,
            "partition should already exist on second call"
        );
    }
}
