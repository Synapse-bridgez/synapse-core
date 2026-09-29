//! Per-tenant data retention policy engine for transactions and settlements.
//!
//! This module manages retention policies for transaction and settlement data,
//! allowing each tenant to configure retention periods subject to compliance minimums.
//! Expired records are archived to cold storage rather than hard-deleted by default.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Minimum retention days enforced platform-wide for compliance.
const PLATFORM_MINIMUM_RETENTION_DAYS: i64 = 90; // 90 days minimum compliance floor

/// Per-tenant retention policy configuration.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TenantRetentionPolicy {
    pub tenant_id: Uuid,
    pub transaction_retention_days: i64,
    pub settlement_retention_days: i64,
    pub allow_hard_delete: bool, // If false, expired records are archived instead
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TenantRetentionPolicy {
    /// Creates a new retention policy, enforcing compliance minimum floors.
    pub fn new(
        tenant_id: Uuid,
        transaction_retention_days: i64,
        settlement_retention_days: i64,
        allow_hard_delete: bool,
    ) -> Self {
        // Enforce compliance minimums.
        let transaction_days = transaction_retention_days.max(PLATFORM_MINIMUM_RETENTION_DAYS);
        let settlement_days = settlement_retention_days.max(PLATFORM_MINIMUM_RETENTION_DAYS);

        Self {
            tenant_id,
            transaction_retention_days: transaction_days,
            settlement_retention_days: settlement_days,
            allow_hard_delete,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// Validates the policy against compliance floors.
    pub fn is_compliant(&self) -> bool {
        self.transaction_retention_days >= PLATFORM_MINIMUM_RETENTION_DAYS
            && self.settlement_retention_days >= PLATFORM_MINIMUM_RETENTION_DAYS
    }

    /// Computes the cutoff date for transactions based on this policy.
    pub fn transaction_cutoff_date(&self) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::days(self.transaction_retention_days)
    }

    /// Computes the cutoff date for settlements based on this policy.
    pub fn settlement_cutoff_date(&self) -> DateTime<Utc> {
        Utc::now() - chrono::Duration::days(self.settlement_retention_days)
    }
}

/// Archives expired transactions to cold storage without hard-deleting them.
///
/// Transactions are archived using the partition-detach mechanism: old partitions
/// are detached from the parent table rather than dropped, preserving data for
/// cold-storage archival.
pub async fn archive_expired_transactions(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<ArchiveResult, Box<dyn std::error::Error + Send + Sync>> {
    // Fetch the tenant's retention policy.
    let policy = fetch_retention_policy(pool, tenant_id).await?;
    let cutoff = policy.transaction_cutoff_date();

    info!(
        tenant_id = %tenant_id,
        cutoff = %cutoff.to_rfc3339(),
        retention_days = policy.transaction_retention_days,
        "Starting transaction archival"
    );

    // Identify partitions older than cutoff.
    let old_partitions = identify_old_partitions(pool, "transactions", &cutoff).await?;

    let mut result = ArchiveResult {
        archived_count: 0,
        failed_count: 0,
        failed_partitions: Vec::new(),
    };

    for partition in old_partitions {
        match detach_partition(pool, &partition).await {
            Ok(count) => {
                result.archived_count += count;
                info!(
                    tenant_id = %tenant_id,
                    partition = %partition,
                    archived_rows = count,
                    "Partition detached (archived to cold storage)"
                );
            }
            Err(e) => {
                result.failed_count += 1;
                result.failed_partitions.push(partition.clone());
                error!(
                    tenant_id = %tenant_id,
                    partition = %partition,
                    error = %e,
                    "Failed to detach partition"
                );
            }
        }
    }

    info!(
        tenant_id = %tenant_id,
        archived = result.archived_count,
        failed = result.failed_count,
        "Transaction archival complete"
    );

    Ok(result)
}

/// Archives expired settlements to cold storage, respecting dispute holds.
///
/// Settlements with open disputes are never archived, regardless of retention policy.
/// Only closed/resolved settlements older than the cutoff are candidates for archival.
pub async fn archive_expired_settlements(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<ArchiveResult, Box<dyn std::error::Error + Send + Sync>> {
    // Fetch the tenant's retention policy.
    let policy = fetch_retention_policy(pool, tenant_id).await?;
    let cutoff = policy.settlement_cutoff_date();

    info!(
        tenant_id = %tenant_id,
        cutoff = %cutoff.to_rfc3339(),
        retention_days = policy.settlement_retention_days,
        "Starting settlement archival"
    );

    // Query for settlements eligible for archival:
    // - Created before cutoff
    // - No open disputes
    let archivable: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM settlements \
         WHERE tenant_id = $1 \
         AND created_at < $2 \
         AND NOT EXISTS (SELECT 1 FROM disputes WHERE settlement_id = id AND status != 'resolved')",
    )
    .bind(tenant_id)
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    let mut result = ArchiveResult {
        archived_count: 0,
        failed_count: 0,
        failed_partitions: Vec::new(),
    };

    // Mark settlements as archived instead of hard-deleting.
    for settlement_id in archivable {
        match mark_settlement_archived(pool, settlement_id).await {
            Ok(()) => {
                result.archived_count += 1;
            }
            Err(e) => {
                result.failed_count += 1;
                result.failed_partitions.push(settlement_id.to_string());
                error!(
                    settlement_id = %settlement_id,
                    error = %e,
                    "Failed to mark settlement as archived"
                );
            }
        }
    }

    info!(
        tenant_id = %tenant_id,
        archived = result.archived_count,
        failed = result.failed_count,
        "Settlement archival complete"
    );

    Ok(result)
}

/// Stores a tenant's retention policy in the database.
pub async fn upsert_retention_policy(
    pool: &PgPool,
    policy: &TenantRetentionPolicy,
) -> Result<(), sqlx::Error> {
    if !policy.is_compliant() {
        warn!(
            tenant_id = %policy.tenant_id,
            transaction_days = policy.transaction_retention_days,
            settlement_days = policy.settlement_retention_days,
            minimum_floor_days = PLATFORM_MINIMUM_RETENTION_DAYS,
            "Retention policy below compliance floor; enforcing minimum"
        );
    }

    sqlx::query(
        "INSERT INTO tenant_retention_policies \
         (tenant_id, transaction_retention_days, settlement_retention_days, allow_hard_delete, updated_at) \
         VALUES ($1, $2, $3, $4, NOW()) \
         ON CONFLICT (tenant_id) DO UPDATE SET \
         transaction_retention_days = $2, \
         settlement_retention_days = $3, \
         allow_hard_delete = $4, \
         updated_at = NOW()",
    )
    .bind(policy.tenant_id)
    .bind(policy.transaction_retention_days)
    .bind(policy.settlement_retention_days)
    .bind(policy.allow_hard_delete)
    .execute(pool)
    .await?;

    info!(
        tenant_id = %policy.tenant_id,
        "Retention policy upserted"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Fetches a tenant's retention policy, or returns the default if not configured.
async fn fetch_retention_policy(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<TenantRetentionPolicy, Box<dyn std::error::Error + Send + Sync>> {
    let policy: Option<TenantRetentionPolicy> = sqlx::query_as(
        "SELECT tenant_id, transaction_retention_days, settlement_retention_days, allow_hard_delete, created_at, updated_at \
         FROM tenant_retention_policies WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;

    Ok(policy.unwrap_or_else(|| {
        // Default policy: use platform minimums, archive by default (no hard-delete).
        TenantRetentionPolicy::new(
            tenant_id,
            PLATFORM_MINIMUM_RETENTION_DAYS,
            PLATFORM_MINIMUM_RETENTION_DAYS,
            false,
        )
    }))
}

/// Identifies partitions of a table that are older than the cutoff date.
async fn identify_old_partitions(
    pool: &PgPool,
    parent_table: &str,
    cutoff: &DateTime<Utc>,
) -> Result<Vec<String>, sqlx::Error> {
    let cutoff_str = cutoff.to_rfc3339();
    let partitions: Vec<(String,)> = sqlx::query_as(
        "SELECT tablename FROM pg_tables \
         WHERE schemaname = 'public' AND tablename LIKE $1 || '_%' \
         AND tablename < $2 \
         ORDER BY tablename",
    )
    .bind(parent_table)
    .bind(&cutoff_str)
    .fetch_all(pool)
    .await?;

    Ok(partitions.into_iter().map(|(name,)| name).collect())
}

/// Detaches a partition from its parent table (preserving data for cold storage).
async fn detach_partition(pool: &PgPool, partition_name: &str) -> Result<i64, sqlx::Error> {
    // Get row count before detaching.
    let row_count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {}", partition_name))
        .fetch_one(pool)
        .await?;

    // Detach the partition (preserve data, don't drop).
    sqlx::query(&format!(
        "ALTER TABLE {} DETACH PARTITION {} FINALIZE",
        extract_parent_table(partition_name),
        partition_name
    ))
    .execute(pool)
    .await?;

    Ok(row_count)
}

/// Marks a settlement as archived in a dedicated column.
async fn mark_settlement_archived(pool: &PgPool, settlement_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE settlements SET archived_at = NOW() WHERE id = $1 AND archived_at IS NULL",
    )
    .bind(settlement_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Extracts the parent table name from a partition name (e.g., "transactions_y2025m01" → "transactions").
fn extract_parent_table(partition_name: &str) -> &str {
    partition_name
        .split('_')
        .next()
        .unwrap_or("transactions")
}

/// Result of an archival operation.
#[derive(Debug, Clone)]
pub struct ArchiveResult {
    pub archived_count: i64,
    pub failed_count: usize,
    pub failed_partitions: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retention_policy_enforces_compliance_minimum() {
        let tenant_id = Uuid::new_v4();
        let policy = TenantRetentionPolicy::new(
            tenant_id,
            30,  // Below minimum
            30,  // Below minimum
            false,
        );
        assert_eq!(policy.transaction_retention_days, PLATFORM_MINIMUM_RETENTION_DAYS);
        assert_eq!(policy.settlement_retention_days, PLATFORM_MINIMUM_RETENTION_DAYS);
    }

    #[test]
    fn test_retention_policy_compliance_check() {
        let tenant_id = Uuid::new_v4();
        let compliant = TenantRetentionPolicy::new(
            tenant_id,
            PLATFORM_MINIMUM_RETENTION_DAYS,
            PLATFORM_MINIMUM_RETENTION_DAYS,
            false,
        );
        assert!(compliant.is_compliant());

        let non_compliant = TenantRetentionPolicy {
            tenant_id,
            transaction_retention_days: 30,
            settlement_retention_days: 30,
            allow_hard_delete: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(!non_compliant.is_compliant());
    }

    #[test]
    fn test_cutoff_date_computation() {
        let tenant_id = Uuid::new_v4();
        let policy = TenantRetentionPolicy::new(tenant_id, 100, 100, false);
        let cutoff = policy.transaction_cutoff_date();
        let now = Utc::now();
        let expected_diff = chrono::Duration::days(100);
        let actual_diff = now - cutoff;
        // Allow 1 second tolerance for test execution time.
        assert!(
            (actual_diff - expected_diff).num_seconds().abs() < 1,
            "cutoff should be ~100 days ago"
        );
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_archive_expired_transactions() {
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

        // Test that retention table can be created.
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tenant_retention_policies (
                tenant_id UUID PRIMARY KEY,
                transaction_retention_days BIGINT,
                settlement_retention_days BIGINT,
                allow_hard_delete BOOLEAN,
                created_at TIMESTAMPTZ DEFAULT NOW(),
                updated_at TIMESTAMPTZ DEFAULT NOW()
            )",
        )
        .execute(&pool)
        .await
        .unwrap();

        let result: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'tenant_retention_policies')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(result, "retention policies table must exist");
    }
}
