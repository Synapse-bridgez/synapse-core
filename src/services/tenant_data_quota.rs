//! # Tenant Data Quota Service (#1287)
//!
//! Extends the existing API-request quota system to also enforce **storage and
//! row-count quotas per tenant**, so a single tenant's unbounded data growth
//! cannot degrade shared database resources for everyone else.
//!
//! ## Architecture
//!
//! * **Schema**: `tenant_data_quotas` (config) + `tenant_data_quota_usage`
//!   (time-series measurements) + `tenant_quota_summary` (convenience view).
//!   See migration `20260927000003_tenant_data_quotas.sql`.
//!
//! * **[`TenantDataQuotaJob`]**: a [`Job`]-implementing scheduled task that
//!   measures each tenant's actual row count and estimated storage size, writes
//!   a snapshot into `tenant_data_quota_usage`, and returns a list of
//!   [`QuotaViolation`]s.  The scheduler wires this at startup (see `main.rs`).
//!
//! * **Write enforcement** ([`check_write_allowed`]): called in the webhook /
//!   callback handler before inserting a new transaction row.  Returns
//!   `Err(QuotaWriteBlockedError)` if the tenant has a hard breach; writes are
//!   allowed (with a log warning) at soft-threshold breaches.
//!
//! * **Admin surface**: the existing `/admin/quotas` and
//!   `/admin/quotas/:tenant_id` handlers are extended (see
//!   `src/handlers/admin/quota.rs`) to include `data_quota` from the
//!   `tenant_quota_summary` view.

use crate::services::scheduler::Job;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Per-tenant data quota configuration (mirrors `tenant_data_quotas` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantDataQuotaConfig {
    pub tenant_id: Uuid,
    /// Maximum transaction rows allowed (`None` = unlimited).
    pub max_row_count: Option<i64>,
    /// Maximum estimated storage in bytes (`None` = unlimited).
    pub max_storage_bytes: Option<i64>,
    /// Fraction at which the soft warning fires (0.0–1.0, default 0.80).
    pub soft_threshold: f64,
}

/// Upsert request for quota admin endpoints.
#[derive(Debug, Clone, Deserialize)]
pub struct SetDataQuotaRequest {
    pub max_row_count: Option<i64>,
    pub max_storage_bytes: Option<i64>,
    /// Soft-warn threshold (0.0–1.0), defaults to 0.80 if omitted.
    pub soft_threshold: Option<f64>,
}

// ---------------------------------------------------------------------------
// Measurement snapshot
// ---------------------------------------------------------------------------

/// A single measured usage snapshot for one tenant.
#[derive(Debug, Clone, Serialize)]
pub struct TenantUsageSnapshot {
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub row_count: i64,
    pub storage_bytes: i64,
    /// Usage as a percentage of the configured row limit (None if no limit set).
    pub row_count_pct: Option<f64>,
    /// Usage as a percentage of the configured storage limit (None if no limit set).
    pub storage_pct: Option<f64>,
    pub row_soft_breach: bool,
    pub row_hard_breach: bool,
    pub storage_soft_breach: bool,
    pub storage_hard_breach: bool,
    pub measured_at: DateTime<Utc>,
}

/// A breach or near-breach condition surfaced by the quota check job.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaViolation {
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub violation_type: ViolationType,
    pub used: i64,
    pub limit: i64,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationType {
    RowCountSoftBreach,
    RowCountHardBreach,
    StorageSoftBreach,
    StorageHardBreach,
}

// ---------------------------------------------------------------------------
// Write enforcement
// ---------------------------------------------------------------------------

/// Error returned when a hard quota breach blocks a write.
#[derive(Debug, Clone, Serialize)]
pub struct QuotaWriteBlockedError {
    pub tenant_id: Uuid,
    pub reason: String,
}

impl std::fmt::Display for QuotaWriteBlockedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "write blocked for tenant {}: {}", self.tenant_id, self.reason)
    }
}

impl std::error::Error for QuotaWriteBlockedError {}

/// Check whether a new write is allowed for `tenant_id`.
///
/// Reads the **latest** usage snapshot from `tenant_data_quota_usage` (written
/// by the background job) rather than measuring live — this is intentionally a
/// cached check so it does not add a full-table COUNT to the hot write path.
///
/// Returns:
/// - `Ok(())` — write is allowed (no quota configured, or usage is below soft
///   threshold).
/// - `Ok(())` with a log warning — soft threshold breached; write still allowed.
/// - `Err(QuotaWriteBlockedError)` — hard threshold breached; write is blocked.
pub async fn check_write_allowed(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<(), QuotaWriteBlockedError> {
    // Use the view to get the latest measurement; if no row exists (no quota
    // configured, or job hasn't run yet) we allow the write.
    let row = sqlx::query(
        r#"
        SELECT
            row_hard_breach,
            storage_hard_breach,
            row_soft_breach,
            storage_soft_breach,
            row_count,
            storage_bytes,
            max_row_count,
            max_storage_bytes
        FROM tenant_quota_summary
        WHERE tenant_id = $1
        "#,
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None); // on DB error, fail open (don't block writes)

    let row = match row {
        Some(r) => r,
        None => return Ok(()), // no quota configured
    };

    let row_hard: bool = row
        .try_get("row_hard_breach")
        .unwrap_or(false);
    let storage_hard: bool = row
        .try_get("storage_hard_breach")
        .unwrap_or(false);
    let row_soft: bool = row
        .try_get("row_soft_breach")
        .unwrap_or(false);
    let storage_soft: bool = row
        .try_get("storage_soft_breach")
        .unwrap_or(false);

    if row_hard {
        let limit: i64 = row.try_get("max_row_count").unwrap_or(0);
        let used: i64 = row.try_get("row_count").unwrap_or(0);
        warn!(
            counter.tenant_quota_write_blocked_total = 1u64,
            tenant_id = %tenant_id,
            dimension = "row_count",
            used,
            limit,
            "write blocked: tenant has exceeded row-count hard quota"
        );
        return Err(QuotaWriteBlockedError {
            tenant_id,
            reason: format!(
                "row-count hard quota exceeded ({} / {} rows); \
                 contact support to increase your quota",
                used, limit
            ),
        });
    }

    if storage_hard {
        let limit: i64 = row.try_get("max_storage_bytes").unwrap_or(0);
        let used: i64 = row.try_get("storage_bytes").unwrap_or(0);
        warn!(
            counter.tenant_quota_write_blocked_total = 1u64,
            tenant_id = %tenant_id,
            dimension = "storage",
            used,
            limit,
            "write blocked: tenant has exceeded storage hard quota"
        );
        return Err(QuotaWriteBlockedError {
            tenant_id,
            reason: format!(
                "storage hard quota exceeded ({} / {} bytes); \
                 contact support to increase your quota",
                used, limit
            ),
        });
    }

    if row_soft {
        let limit: i64 = row.try_get("max_row_count").unwrap_or(0);
        let used: i64 = row.try_get("row_count").unwrap_or(0);
        warn!(
            counter.tenant_quota_soft_breach_total = 1u64,
            tenant_id = %tenant_id,
            dimension = "row_count",
            used,
            limit,
            "tenant is approaching row-count hard quota (soft threshold breached); \
             write allowed"
        );
    }

    if storage_soft {
        let limit: i64 = row.try_get("max_storage_bytes").unwrap_or(0);
        let used: i64 = row.try_get("storage_bytes").unwrap_or(0);
        warn!(
            counter.tenant_quota_soft_breach_total = 1u64,
            tenant_id = %tenant_id,
            dimension = "storage",
            used,
            limit,
            "tenant is approaching storage hard quota (soft threshold breached); \
             write allowed"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Admin helpers: configure and read quotas
// ---------------------------------------------------------------------------

/// Upsert a data quota configuration for `tenant_id`.
pub async fn set_data_quota(
    pool: &PgPool,
    tenant_id: Uuid,
    req: &SetDataQuotaRequest,
) -> Result<(), sqlx::Error> {
    let soft = req.soft_threshold.unwrap_or(0.80).min(1.0).max(0.0);

    sqlx::query(
        r#"
        INSERT INTO tenant_data_quotas
            (tenant_id, max_row_count, max_storage_bytes, soft_threshold, updated_at)
        VALUES ($1, $2, $3, $4, now())
        ON CONFLICT (tenant_id) DO UPDATE
            SET max_row_count     = EXCLUDED.max_row_count,
                max_storage_bytes = EXCLUDED.max_storage_bytes,
                soft_threshold    = EXCLUDED.soft_threshold,
                updated_at        = now()
        "#,
    )
    .bind(tenant_id)
    .bind(req.max_row_count)
    .bind(req.max_storage_bytes)
    .bind(soft)
    .execute(pool)
    .await?;

    info!(
        tenant_id = %tenant_id,
        max_row_count = ?req.max_row_count,
        max_storage_bytes = ?req.max_storage_bytes,
        soft_threshold = soft,
        "tenant data quota configured"
    );

    Ok(())
}

/// Fetch the latest usage snapshot for all active tenants from the
/// `tenant_quota_summary` view.
pub async fn get_all_quota_summaries(
    pool: &PgPool,
) -> Result<Vec<TenantUsageSnapshot>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT
            tenant_id,
            tenant_name,
            COALESCE(row_count, 0)       AS row_count,
            COALESCE(storage_bytes, 0)   AS storage_bytes,
            row_count_pct::float8,
            storage_pct::float8,
            COALESCE(row_soft_breach, false)     AS row_soft_breach,
            COALESCE(row_hard_breach, false)     AS row_hard_breach,
            COALESCE(storage_soft_breach, false) AS storage_soft_breach,
            COALESCE(storage_hard_breach, false) AS storage_hard_breach,
            COALESCE(last_measured_at, now())    AS measured_at,
            max_row_count,
            max_storage_bytes
        FROM tenant_quota_summary
        ORDER BY tenant_name
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut snapshots = Vec::with_capacity(rows.len());
    for row in &rows {
        snapshots.push(TenantUsageSnapshot {
            tenant_id: row.try_get("tenant_id")?,
            tenant_name: row.try_get("tenant_name")?,
            row_count: row.try_get("row_count")?,
            storage_bytes: row.try_get("storage_bytes")?,
            row_count_pct: row.try_get("row_count_pct")?,
            storage_pct: row.try_get("storage_pct")?,
            row_soft_breach: row.try_get("row_soft_breach")?,
            row_hard_breach: row.try_get("row_hard_breach")?,
            storage_soft_breach: row.try_get("storage_soft_breach")?,
            storage_hard_breach: row.try_get("storage_hard_breach")?,
            measured_at: row.try_get("measured_at")?,
        });
    }

    Ok(snapshots)
}

/// Fetch the latest usage snapshot for a single tenant.
pub async fn get_quota_summary(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<TenantUsageSnapshot>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT
            tenant_id,
            tenant_name,
            COALESCE(row_count, 0)       AS row_count,
            COALESCE(storage_bytes, 0)   AS storage_bytes,
            row_count_pct::float8,
            storage_pct::float8,
            COALESCE(row_soft_breach, false)     AS row_soft_breach,
            COALESCE(row_hard_breach, false)     AS row_hard_breach,
            COALESCE(storage_soft_breach, false) AS storage_soft_breach,
            COALESCE(storage_hard_breach, false) AS storage_hard_breach,
            COALESCE(last_measured_at, now())    AS measured_at
        FROM tenant_quota_summary
        WHERE tenant_id = $1
        "#,
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Ok(None),
        Some(row) => Ok(Some(TenantUsageSnapshot {
            tenant_id: row.try_get("tenant_id")?,
            tenant_name: row.try_get("tenant_name")?,
            row_count: row.try_get("row_count")?,
            storage_bytes: row.try_get("storage_bytes")?,
            row_count_pct: row.try_get("row_count_pct")?,
            storage_pct: row.try_get("storage_pct")?,
            row_soft_breach: row.try_get("row_soft_breach")?,
            row_hard_breach: row.try_get("row_hard_breach")?,
            storage_soft_breach: row.try_get("storage_soft_breach")?,
            storage_hard_breach: row.try_get("storage_hard_breach")?,
            measured_at: row.try_get("measured_at")?,
        })),
    }
}

// ---------------------------------------------------------------------------
// Scheduled measurement job
// ---------------------------------------------------------------------------

/// Background job that measures actual usage for every active tenant and
/// writes a snapshot to `tenant_data_quota_usage`.  Runs on a configurable
/// cron schedule (default: every 15 minutes).
pub struct TenantDataQuotaJob {
    pool: PgPool,
    /// Cron expression (defaults to every 15 minutes).
    schedule: String,
}

impl TenantDataQuotaJob {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            schedule: "0 */15 * * * *".to_string(), // every 15 minutes
        }
    }

    /// Override the cron schedule (useful in tests).
    pub fn with_schedule(mut self, schedule: impl Into<String>) -> Self {
        self.schedule = schedule.into();
        self
    }

    /// Measure all tenants and return the list of violations found.
    pub async fn measure_all(&self) -> Result<Vec<QuotaViolation>, Box<dyn std::error::Error + Send + Sync>> {
        // 1. Load all tenants that have a quota config.
        let configs = sqlx::query(
            r#"
            SELECT
                q.tenant_id,
                t.name                  AS tenant_name,
                q.max_row_count,
                q.max_storage_bytes,
                q.soft_threshold::float8 AS soft_threshold
            FROM tenant_data_quotas q
            JOIN tenants t USING (tenant_id)
            WHERE t.is_active = true
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut violations: Vec<QuotaViolation> = Vec::new();

        for cfg in &configs {
            let tenant_id: Uuid = cfg.try_get("tenant_id")?;
            let tenant_name: String = cfg.try_get("tenant_name")?;
            let max_rows: Option<i64> = cfg.try_get("max_row_count")?;
            let max_bytes: Option<i64> = cfg.try_get("max_storage_bytes")?;
            let soft: f64 = cfg.try_get("soft_threshold")?;

            // 2. Count rows. Use an exact COUNT(*) for accurate enforcement.
            let row_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM transactions WHERE tenant_id = $1",
            )
            .bind(tenant_id)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

            // 3. Estimate storage in bytes using pg_total_relation_size on
            //    the transactions table.  This is a shared estimate across all
            //    tenants in the same physical table; a per-tenant byte
            //    breakdown would require pg_column_size aggregation which is
            //    too expensive on the hot path.  We store a proportional share
            //    based on row count.
            let total_storage: i64 = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(pg_total_relation_size('transactions'), 0)",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);

            let total_rows: i64 = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(reltuples::bigint, 0) FROM pg_class WHERE relname = 'transactions'",
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(1)
            .max(1);

            let storage_bytes = (total_storage as f64 * (row_count as f64 / total_rows as f64)) as i64;

            // 4. Derive percentages and breach flags.
            let (row_pct, row_soft_breach, row_hard_breach) = match max_rows {
                None => (None, false, false),
                Some(limit) if limit > 0 => {
                    let pct = (row_count as f64 / limit as f64) * 100.0;
                    (Some(pct), pct >= soft * 100.0, row_count >= limit)
                }
                _ => (None, false, false),
            };

            let (storage_pct, storage_soft_breach, storage_hard_breach) = match max_bytes {
                None => (None, false, false),
                Some(limit) if limit > 0 => {
                    let pct = (storage_bytes as f64 / limit as f64) * 100.0;
                    (Some(pct), pct >= soft * 100.0, storage_bytes >= limit)
                }
                _ => (None, false, false),
            };

            // 5. Write snapshot.
            sqlx::query(
                r#"
                INSERT INTO tenant_data_quota_usage
                    (tenant_id, row_count, storage_bytes,
                     row_count_pct, storage_pct,
                     row_soft_breach, row_hard_breach,
                     storage_soft_breach, storage_hard_breach)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                "#,
            )
            .bind(tenant_id)
            .bind(row_count)
            .bind(storage_bytes)
            .bind(row_pct.map(|p| p as f32))
            .bind(storage_pct.map(|p| p as f32))
            .bind(row_soft_breach)
            .bind(row_hard_breach)
            .bind(storage_soft_breach)
            .bind(storage_hard_breach)
            .execute(&self.pool)
            .await?;

            // 6. Collect violations for caller.
            if row_hard_breach {
                violations.push(QuotaViolation {
                    tenant_id,
                    tenant_name: tenant_name.clone(),
                    violation_type: ViolationType::RowCountHardBreach,
                    used: row_count,
                    limit: max_rows.unwrap_or(0),
                    pct: row_pct.unwrap_or(0.0),
                });
                warn!(
                    counter.tenant_quota_hard_breach_total = 1u64,
                    tenant_id = %tenant_id,
                    dimension = "row_count",
                    row_count,
                    limit = max_rows,
                    "tenant data quota hard breach: row count"
                );
            } else if row_soft_breach {
                violations.push(QuotaViolation {
                    tenant_id,
                    tenant_name: tenant_name.clone(),
                    violation_type: ViolationType::RowCountSoftBreach,
                    used: row_count,
                    limit: max_rows.unwrap_or(0),
                    pct: row_pct.unwrap_or(0.0),
                });
                warn!(
                    counter.tenant_quota_soft_breach_total = 1u64,
                    tenant_id = %tenant_id,
                    dimension = "row_count",
                    row_count,
                    limit = max_rows,
                    "tenant data quota soft breach: row count approaching limit"
                );
            }

            if storage_hard_breach {
                violations.push(QuotaViolation {
                    tenant_id,
                    tenant_name: tenant_name.clone(),
                    violation_type: ViolationType::StorageHardBreach,
                    used: storage_bytes,
                    limit: max_bytes.unwrap_or(0),
                    pct: storage_pct.unwrap_or(0.0),
                });
                warn!(
                    counter.tenant_quota_hard_breach_total = 1u64,
                    tenant_id = %tenant_id,
                    dimension = "storage",
                    storage_bytes,
                    limit = max_bytes,
                    "tenant data quota hard breach: storage"
                );
            } else if storage_soft_breach {
                violations.push(QuotaViolation {
                    tenant_id,
                    tenant_name: tenant_name.clone(),
                    violation_type: ViolationType::StorageSoftBreach,
                    used: storage_bytes,
                    limit: max_bytes.unwrap_or(0),
                    pct: storage_pct.unwrap_or(0.0),
                });
                warn!(
                    counter.tenant_quota_soft_breach_total = 1u64,
                    tenant_id = %tenant_id,
                    dimension = "storage",
                    storage_bytes,
                    limit = max_bytes,
                    "tenant data quota soft breach: storage approaching limit"
                );
            }

            info!(
                tenant_id = %tenant_id,
                row_count,
                storage_bytes,
                row_hard_breach,
                storage_hard_breach,
                "tenant data quota measured"
            );
        }

        Ok(violations)
    }
}

#[async_trait]
impl Job for TenantDataQuotaJob {
    fn name(&self) -> &str {
        "tenant_data_quota"
    }

    fn schedule(&self) -> &str {
        &self.schedule
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let violations = self.measure_all().await?;

        if violations.is_empty() {
            info!("tenant_data_quota: all tenants within quota limits");
        } else {
            warn!(
                violation_count = violations.len(),
                "tenant_data_quota: {} quota violation(s) detected",
                violations.len()
            );
        }

        Ok(())
    }
}
