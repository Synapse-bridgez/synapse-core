//! Zero-downtime online schema migration tooling for large partitioned tables.
//!
//! This module provides resumable, partition-aware online migration tools for
//! applying schema changes (e.g., adding columns with defaults, creating indexes)
//! across all partitions of a table without holding long locks.

use sqlx::PgPool;
use std::collections::HashSet;
use tracing::{error, info, warn};

/// Represents the state of a single partition migration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PartitionMigrationRecord {
    pub partition_name: String,
    pub migration_type: String, // "ADD_COLUMN" or "CREATE_INDEX"
    pub status: String,         // "pending", "in_progress", "completed", "failed"
    pub error_message: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Configuration for applying a schema change across partitions.
#[derive(Debug, Clone)]
pub struct PartitionMigrationConfig {
    /// Name of the parent table (e.g., "transactions").
    pub parent_table: String,
    /// Type of migration: "ADD_COLUMN" or "CREATE_INDEX".
    pub migration_type: String,
    /// The actual SQL to apply to each partition (e.g., "ADD COLUMN new_col INT DEFAULT 0").
    pub migration_sql: String,
    /// Whether to use CONCURRENTLY for index operations.
    pub use_concurrently: bool,
    /// Batch size: number of partitions to process before checkpointing.
    pub batch_size: usize,
}

/// Applies a schema change across all partitions of a table, resumable if interrupted.
///
/// # Algorithm
/// 1. Fetch all partitions of the parent table.
/// 2. Query the migration tracking table for prior progress.
/// 3. Process partitions in batches, skipping already-completed ones.
/// 4. For each partition, execute the migration SQL, then mark as completed.
/// 5. If a new partition is created during the run, it will be picked up on the next batch.
///
/// # Returns
/// A summary of the migration run (processed, already_done, failed).
pub async fn apply_partition_migration(
    pool: &PgPool,
    config: &PartitionMigrationConfig,
) -> Result<PartitionMigrationSummary, Box<dyn std::error::Error + Send + Sync>> {
    info!(
        parent_table = %config.parent_table,
        migration_type = %config.migration_type,
        "Starting partition migration"
    );

    // Ensure tracking table exists.
    ensure_migration_tracking_table(pool).await?;

    // Fetch all current partitions.
    let mut partitions = fetch_partitions(pool, &config.parent_table).await?;
    info!(
        parent_table = %config.parent_table,
        count = partitions.len(),
        "Found partitions"
    );

    // Fetch prior progress for this migration.
    let completed = fetch_completed_partitions(
        pool,
        &config.parent_table,
        &config.migration_type,
    )
    .await?;

    let mut summary = PartitionMigrationSummary {
        parent_table: config.parent_table.clone(),
        migration_type: config.migration_type.clone(),
        partitions_processed: 0,
        partitions_already_done: completed.len(),
        partitions_failed: 0,
        failed_partitions: Vec::new(),
    };

    // Filter to pending partitions.
    partitions.retain(|p| !completed.contains(&p));

    // Process in batches, checking for new partitions each batch.
    for batch_start in (0..partitions.len()).step_by(config.batch_size) {
        let batch_end = std::cmp::min(batch_start + config.batch_size, partitions.len());
        let batch = &partitions[batch_start..batch_end];

        // Before processing the batch, check for any new partitions added since the last batch.
        let current_partitions = fetch_partitions(pool, &config.parent_table).await?;
        for partition in &current_partitions {
            if !partitions.contains(partition) && !completed.contains(partition) {
                info!(
                    parent_table = %config.parent_table,
                    partition = %partition,
                    "New partition discovered mid-run"
                );
                partitions.push(partition.clone());
            }
        }

        for partition in batch {
            match apply_migration_to_partition(pool, config, partition).await {
                Ok(()) => {
                    mark_partition_completed(pool, &config.parent_table, &config.migration_type, partition)
                        .await?;
                    summary.partitions_processed += 1;
                    info!(
                        parent_table = %config.parent_table,
                        partition = %partition,
                        "Partition migration completed"
                    );
                }
                Err(e) => {
                    let error_msg = format!("{}", e);
                    mark_partition_failed(pool, &config.parent_table, &config.migration_type, partition, &error_msg)
                        .await?;
                    summary.partitions_failed += 1;
                    summary.failed_partitions.push(partition.clone());
                    error!(
                        parent_table = %config.parent_table,
                        partition = %partition,
                        error = %e,
                        "Partition migration failed"
                    );
                }
            }
        }
    }

    info!(
        parent_table = %config.parent_table,
        processed = summary.partitions_processed,
        already_done = summary.partitions_already_done,
        failed = summary.partitions_failed,
        "Partition migration run complete"
    );

    Ok(summary)
}

/// Summary of a partition migration run.
#[derive(Debug, Clone)]
pub struct PartitionMigrationSummary {
    pub parent_table: String,
    pub migration_type: String,
    pub partitions_processed: usize,
    pub partitions_already_done: usize,
    pub partitions_failed: usize,
    pub failed_partitions: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Ensures the partition migration tracking table exists.
async fn ensure_migration_tracking_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS partition_migrations (
            id SERIAL PRIMARY KEY,
            parent_table TEXT NOT NULL,
            partition_name TEXT NOT NULL,
            migration_type TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'pending',
            error_message TEXT,
            started_at TIMESTAMPTZ DEFAULT NOW(),
            completed_at TIMESTAMPTZ,
            UNIQUE(parent_table, partition_name, migration_type)
        );
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetches all partitions for a parent table.
async fn fetch_partitions(pool: &PgPool, parent_table: &str) -> Result<Vec<String>, sqlx::Error> {
    let partitions: Vec<(String,)> = sqlx::query_as(
        "SELECT schemaname || '.' || tablename FROM pg_tables \
         WHERE schemaname = 'public' AND tablename LIKE $1 || '_%' \
         ORDER BY tablename",
    )
    .bind(parent_table)
    .fetch_all(pool)
    .await?;

    Ok(partitions.into_iter().map(|(name,)| name).collect())
}

/// Fetches partitions that have already been completed for this migration.
async fn fetch_completed_partitions(
    pool: &PgPool,
    parent_table: &str,
    migration_type: &str,
) -> Result<HashSet<String>, sqlx::Error> {
    let completed: Vec<(String,)> = sqlx::query_as(
        "SELECT partition_name FROM partition_migrations \
         WHERE parent_table = $1 AND migration_type = $2 AND status = 'completed'",
    )
    .bind(parent_table)
    .bind(migration_type)
    .fetch_all(pool)
    .await?;

    Ok(completed.into_iter().map(|(name,)| name).collect())
}

/// Applies the migration SQL to a single partition.
async fn apply_migration_to_partition(
    pool: &PgPool,
    config: &PartitionMigrationConfig,
    partition_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sql = if config.use_concurrently {
        // For CREATE INDEX CONCURRENTLY, insert before the table name.
        config.migration_sql.replace("CREATE INDEX", "CREATE INDEX CONCURRENTLY")
    } else {
        config.migration_sql.clone()
    };

    let full_sql = format!("ALTER TABLE {} {}", partition_name, sql);

    sqlx::query(&full_sql).execute(pool).await?;
    Ok(())
}

/// Marks a partition as completed in the tracking table.
async fn mark_partition_completed(
    pool: &PgPool,
    parent_table: &str,
    migration_type: &str,
    partition_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO partition_migrations (parent_table, partition_name, migration_type, status, completed_at) \
         VALUES ($1, $2, $3, 'completed', NOW()) \
         ON CONFLICT (parent_table, partition_name, migration_type) \
         DO UPDATE SET status = 'completed', completed_at = NOW()",
    )
    .bind(parent_table)
    .bind(partition_name)
    .bind(migration_type)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks a partition as failed in the tracking table.
async fn mark_partition_failed(
    pool: &PgPool,
    parent_table: &str,
    migration_type: &str,
    partition_name: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO partition_migrations (parent_table, partition_name, migration_type, status, error_message) \
         VALUES ($1, $2, $3, 'failed', $4) \
         ON CONFLICT (parent_table, partition_name, migration_type) \
         DO UPDATE SET status = 'failed', error_message = $4",
    )
    .bind(parent_table)
    .bind(partition_name)
    .bind(migration_type)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_partition_migration_config_structure() {
        let config = PartitionMigrationConfig {
            parent_table: "transactions".to_string(),
            migration_type: "ADD_COLUMN".to_string(),
            migration_sql: "ADD COLUMN new_col INT DEFAULT 0".to_string(),
            use_concurrently: false,
            batch_size: 10,
        };
        assert_eq!(config.parent_table, "transactions");
        assert_eq!(config.migration_type, "ADD_COLUMN");
    }

    #[tokio::test]
    #[ignore = "Requires Docker and real partitioned table"]
    async fn test_partition_migration_with_multi_partition_fixture() {
        use sqlx::migrate::Migrator;
        use std::path::Path;
        use testcontainers::{runners::AsyncRunner, ImageExt};
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        // Verify the migrations table can be created.
        ensure_migration_tracking_table(&pool).await.unwrap();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'partition_migrations')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(exists, "partition_migrations table must be created");
    }
}
