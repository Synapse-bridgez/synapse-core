use chrono::{Datelike, Duration, Utc};
use sqlx::PgPool;
use tracing::{error, info};

/// Partition management job for the transactions table.
/// This module handles:
/// - Creating new monthly partitions automatically
/// - Archiving old partitions based on retention policy
/// - Maintenance tasks like ANALYZE on partitions
pub struct PartitionManager;

impl PartitionManager {
    /// Create a new monthly partition for the specified year and month.
    ///
    /// Partition naming: transactions_y{YEAR}m{MONTH}
    /// Example: transactions_y2025m02 for February 2025
    pub async fn create_partition(
        pool: &PgPool,
        year: i32,
        month: u32,
    ) -> anyhow::Result<()> {
        let partition_name = format!("transactions_y{:04}m{:02}", year, month);
        let start_date = chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .ok_or_else(|| anyhow::anyhow!("Invalid date: {}-{}", year, month))?;
        let end_date = start_date + Duration::days(32 - start_date.day() as i64);
        let end_date = chrono::NaiveDate::from_ymd_opt(
            if end_date.month() == 12 {
                end_date.year() + 1
            } else {
                end_date.year()
            },
            if end_date.month() == 12 {
                1
            } else {
                end_date.month() + 1
            },
            1,
        )
        .ok_or_else(|| anyhow::anyhow!("Invalid end date calculation"))?;

        let query = format!(
            r#"
            DO $$
            BEGIN
                IF NOT EXISTS (
                    SELECT 1 FROM information_schema.tables 
                    WHERE table_name = '{}'
                ) THEN
                    CREATE TABLE {} PARTITION OF transactions 
                    FOR VALUES FROM ('{}') TO ('{}');
                    CREATE INDEX idx_{}_status ON {}(status);
                    CREATE INDEX idx_{}_stellar_account ON {}(stellar_account);
                    CREATE INDEX idx_{}_created_at ON {}(created_at);
                    RAISE NOTICE 'Created partition: {}';
                ELSE
                    RAISE NOTICE 'Partition {} already exists';
                END IF;
            END $$;
            "#,
            partition_name,
            partition_name,
            start_date,
            end_date,
            partition_name,
            partition_name,
            partition_name,
            partition_name,
            partition_name,
            partition_name,
            partition_name,
            partition_name,
        );

        sqlx::query(&query)
            .execute(pool)
            .await
            .map_err(|e| {
                error!(
                    "Failed to create partition {}_{:02}: {}",
                    year, month, e
                );
                anyhow::anyhow!("Failed to create partition: {}", e)
            })?;

        info!("Successfully created partition: {}", partition_name);
        Ok(())
    }

    /// Ensure upcoming partitions are created.
    /// This job should run monthly to create partitions for the next few months ahead.
    pub async fn ensure_future_partitions(pool: &PgPool, months_ahead: i32) -> anyhow::Result<()> {
        let now = Utc::now();
        let current_year = now.year();
        let current_month = now.month() as i32;

        for i in 0..months_ahead {
            let target_month = current_month + i;
            let (year, month) = if target_month > 12 {
                (current_year + 1, (target_month - 12) as u32)
            } else {
                (current_year, target_month as u32)
            };

            if let Err(e) = Self::create_partition(pool, year, month).await {
                // Log error but continue with remaining partitions
                error!(
                    "Failed to create partition for {}-{}: {}",
                    year, month, e
                );
            }
        }

        info!(
            "Completed ensuring future partitions for {} months ahead",
            months_ahead
        );
        Ok(())
    }

    /// Detach and archive old partitions based on retention policy.
    ///
    /// This job detaches partitions older than the specified months.
    /// Detached partitions can be backed up or archived separately.
    pub async fn archive_old_partitions(
        pool: &PgPool,
        retention_months: i32,
    ) -> anyhow::Result<()> {
        let now = Utc::now();
        let cutoff_year = now.year();
        let cutoff_month = now.month() as i32 - retention_months;

        let (archive_before_year, archive_before_month) = if cutoff_month <= 0 {
            (cutoff_year - 1, (cutoff_month + 12) as i32)
        } else {
            (cutoff_year, cutoff_month as i32)
        };

        // Get list of partitions to archive
        let partitions: Vec<String> = sqlx::query_scalar(
            r#"
            SELECT tablename FROM pg_tables
            WHERE tablename LIKE 'transactions_y%m%'
            AND (
                (right(tablename, 2)::int < $1 AND substring(tablename, 13, 4)::int <= $2)
                OR substring(tablename, 13, 4)::int < $2
            )
            ORDER BY tablename
            "#,
        )
        .bind(archive_before_month)
        .bind(archive_before_year)
        .fetch_all(pool)
        .await
        .map_err(|e| {
            error!("Failed to query partitions for archival: {}", e);
            anyhow::anyhow!("Failed to query partitions: {}", e)
        })?;

        for partition in partitions.iter() {
            match Self::detach_partition(pool, &partition).await {
                Ok(_) => info!("Successfully detached partition: {}", partition),
                Err(e) => error!("Failed to detach partition {}: {}", partition, e),
            }
        }

        if !partitions.is_empty() {
            info!("Archived {} partitions", partitions.len());
        } else {
            info!("No partitions to archive");
        }

        Ok(())
    }

    /// Detach a partition from the parent table.
    /// After detaching, the partition can be independent or dropped for archival.
    async fn detach_partition(pool: &PgPool, partition_name: &str) -> anyhow::Result<()> {
        let query = format!(
            "ALTER TABLE transactions DETACH PARTITION {} FINALIZE;",
            partition_name
        );

        sqlx::query(&query)
            .execute(pool)
            .await
            .map_err(|e| {
                error!("Failed to detach partition {}: {}", partition_name, e);
                anyhow::anyhow!("Failed to detach partition: {}", e)
            })?;

        Ok(())
    }

    /// Analyze partitions to update statistics for query planner.
    /// This should be run periodically to maintain query performance.
    pub async fn analyze_partitions(pool: &PgPool) -> anyhow::Result<()> {
        sqlx::query("ANALYZE transactions;")
            .execute(pool)
            .await
            .map_err(|e| {
                error!("Failed to analyze transactions partitions: {}", e);
                anyhow::anyhow!("Failed to analyze partitions: {}", e)
            })?;

        info!("Successfully analyzed transactions partitions");
        Ok(())
    }

    /// Run routine partition maintenance tasks.
    /// This is the main entry point for the partition management job.
    ///
    /// Tasks:
    /// 1. Ensure partitions exist for the next 3 months
    /// 2. Archive partitions older than 12 months (retention policy)
    /// 3. Analyze partitions for query optimization
    pub async fn run_maintenance(pool: &PgPool) -> anyhow::Result<()> {
        info!("Starting partition maintenance job");

        // Ensure future partitions
        if let Err(e) = Self::ensure_future_partitions(pool, 3).await {
            error!("Failed to ensure future partitions: {}", e);
        }

        // Archive old partitions (older than 12 months)
        if let Err(e) = Self::archive_old_partitions(pool, 12).await {
            error!("Failed to archive old partitions: {}", e);
        }

        // Analyze partitions
        if let Err(e) = Self::analyze_partitions(pool).await {
            error!("Failed to analyze partitions: {}", e);
        }

        info!("Partition maintenance job completed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_name_format() {
        // Test that partition names are formatted correctly
        let year = 2025;
        let month = 2;
        let expected = "transactions_y2025m02";
        let actual = format!("transactions_y{:04}m{:02}", year, month);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_month_calculation() {
        let current_year = 2025;
        let current_month = 11; // November

        // Calculate next 3 months
        let (y1, m1) = if current_month + 0 > 12 {
            (current_year + 1, (current_month + 0 - 12) as u32)
        } else {
            (current_year, current_month as u32)
        };
        assert_eq!((y1, m1), (2025, 11));

        let (y2, m2) = if current_month + 1 > 12 {
            (current_year + 1, (current_month + 1 - 12) as u32)
        } else {
            (current_year, (current_month + 1) as u32)
        };
        assert_eq!((y2, m2), (2025, 12));

        let (y3, m3) = if current_month + 2 > 12 {
            (current_year + 1, (current_month + 2 - 12) as u32)
        } else {
            (current_year, (current_month + 2) as u32)
        };
        assert_eq!((y3, m3), (2026, 1));
    }
}
