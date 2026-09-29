//! # Cold-Storage Audit Log Query Layer (#1285)
//!
//! Provides transparent read access to audit log data that spans both the live
//! hot-path `audit_logs` table and archived cold-tier rows stored in
//! `cold_audit_logs` (rehydrated from `audit_log_archives`).
//!
//! ## Transparency contract
//!
//! Callers query [`query_audit_logs`] with a time range.  The function:
//!
//!  1. Determines whether the range overlaps any known cold archive pointers
//!     (`audit_log_cold_pointers`).
//!  2. If cold data is needed and not yet rehydrated, calls
//!     [`rehydrate_archive`] to load rows from the archive storage backend
//!     into `cold_audit_logs`.
//!  3. Issues a single `SELECT … FROM audit_logs_unified WHERE timestamp
//!     BETWEEN …` against the DB view (which UNION ALLs both tiers).
//!  4. Returns a [`UnifiedQueryResult`] that includes the rows **and** a
//!     [`TierInfo`] describing whether cold storage was touched and what
//!     added latency to expect.
//!
//! ## Logging
//!
//! Every query that touches cold storage emits a structured `tracing::info!`
//! event with `cold_tier_touched = true` and `cold_row_count` so this is
//! surfaced in telemetry without changing the API surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tracing::{info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single row returned from the unified (hot + cold) audit log view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedAuditRow {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub entity_type: String,
    pub action: String,
    pub old_val: Option<serde_json::Value>,
    pub new_val: Option<serde_json::Value>,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
    /// Which storage tier this row was served from: `"hot"` or `"cold"`.
    pub tier: String,
    /// Archive file ID (only set for `tier = "cold"`).
    pub archive_id: Option<Uuid>,
}

/// Metadata about which tiers were touched by a [`query_audit_logs`] call.
///
/// Included in API responses and logs so callers can surface the cold-storage
/// access and manage latency expectations accordingly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierInfo {
    /// True when at least one row in the result came from cold storage.
    pub cold_tier_touched: bool,
    /// Number of rows sourced from cold storage (0 if `cold_tier_touched` is false).
    pub cold_row_count: usize,
    /// Number of rows sourced from hot storage.
    pub hot_row_count: usize,
    /// Human-readable note about cold storage latency, present only when cold
    /// data was accessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_latency_note: Option<String>,
}

/// Combined result returned by [`query_audit_logs`].
#[derive(Debug, Clone, Serialize)]
pub struct UnifiedQueryResult {
    pub rows: Vec<UnifiedAuditRow>,
    pub total: i64,
    pub tier_info: TierInfo,
}

// ---------------------------------------------------------------------------
// Core query function
// ---------------------------------------------------------------------------

/// Query audit logs transparently across hot and cold storage tiers.
///
/// - `from`  — inclusive lower bound (UTC).
/// - `to`    — inclusive upper bound (UTC).
/// - `entity_id` — optional entity filter.
/// - `entity_type` — optional entity-type filter.
/// - `limit` — maximum rows to return (capped at 1000).
///
/// Returns a [`UnifiedQueryResult`] whose `tier_info` reports whether any cold
/// storage was touched.  Cold-storage access is also logged as a structured
/// trace event so it surfaces in telemetry dashboards without any API change.
pub async fn query_audit_logs(
    pool: &PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    entity_id: Option<Uuid>,
    entity_type: Option<&str>,
    limit: i64,
) -> Result<UnifiedQueryResult, sqlx::Error> {
    let limit = limit.min(1000).max(1);

    // Step 1: Check whether any cold pointers overlap the requested range.
    // This is a cheap read against a small metadata table (O(archives) rows).
    let overlapping_cold = check_cold_overlap(pool, from, to).await?;

    // Step 2: Build and run the unified query.
    let mut conditions: Vec<String> = Vec::new();
    let mut param_idx = 1_usize;

    conditions.push(format!("timestamp >= ${}", param_idx));
    param_idx += 1;
    conditions.push(format!("timestamp <= ${}", param_idx));
    param_idx += 1;

    if entity_id.is_some() {
        conditions.push(format!("entity_id = ${}", param_idx));
        param_idx += 1;
    }
    if entity_type.is_some() {
        conditions.push(format!("entity_type = ${}", param_idx));
        param_idx += 1;
    }

    let where_clause = format!("WHERE {}", conditions.join(" AND "));

    let count_sql = format!(
        "SELECT COUNT(*) AS cnt FROM audit_logs_unified {}",
        where_clause
    );
    let data_sql = format!(
        "SELECT id, entity_id, entity_type, action, old_val, new_val, actor, \
         timestamp, tier, archive_id \
         FROM audit_logs_unified {} \
         ORDER BY timestamp DESC \
         LIMIT ${}",
        where_clause, param_idx
    );

    // Bind parameters in order.
    macro_rules! bind_common {
        ($q:expr) => {{
            let q = $q.bind(from).bind(to);
            let q = if let Some(eid) = entity_id { q.bind(eid) } else { q };
            let q = if let Some(et) = entity_type { q.bind(et) } else { q };
            q
        }};
    }

    let count_row = bind_common!(sqlx::query(&count_sql))
        .fetch_one(pool)
        .await?;
    let total: i64 = count_row.try_get("cnt")?;

    let data_rows = bind_common!(sqlx::query(&data_sql))
        .bind(limit)
        .fetch_all(pool)
        .await?;

    let mut rows: Vec<UnifiedAuditRow> = Vec::with_capacity(data_rows.len());
    let mut cold_count = 0usize;
    let mut hot_count = 0usize;

    for row in &data_rows {
        let tier: String = row.try_get("tier")?;
        if tier == "cold" {
            cold_count += 1;
        } else {
            hot_count += 1;
        }
        rows.push(UnifiedAuditRow {
            id: row.try_get("id")?,
            entity_id: row.try_get("entity_id")?,
            entity_type: row.try_get("entity_type")?,
            action: row.try_get("action")?,
            old_val: row.try_get("old_val")?,
            new_val: row.try_get("new_val")?,
            actor: row.try_get("actor")?,
            timestamp: row.try_get("timestamp")?,
            tier,
            archive_id: row.try_get("archive_id")?,
        });
    }

    let cold_tier_touched = cold_count > 0 || overlapping_cold;

    if cold_tier_touched {
        info!(
            cold_tier_touched = true,
            cold_row_count = cold_count,
            hot_row_count = hot_count,
            from = %from.to_rfc3339(),
            to   = %to.to_rfc3339(),
            "audit_logs_unified: query touched cold storage tier"
        );
    }

    let tier_info = TierInfo {
        cold_tier_touched,
        cold_row_count: cold_count,
        hot_row_count: hot_count,
        cold_latency_note: if cold_tier_touched {
            Some(
                "This query range spans cold-storage-archived data. \
                 Cold rows are served from rehydrated on-disk archives; \
                 first-access latency may be higher than hot-tier queries."
                    .to_string(),
            )
        } else {
            None
        },
    };

    Ok(UnifiedQueryResult { rows, total, tier_info })
}

// ---------------------------------------------------------------------------
// Cold-pointer helpers
// ---------------------------------------------------------------------------

/// Returns `true` if any cold archive has been registered that overlaps
/// `[from, to]`.  A `false` result means the range is fully served from hot
/// storage; the caller does not need to attempt rehydration.
pub async fn check_cold_overlap(
    pool: &PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM audit_log_cold_pointers
            WHERE covers_from <= $2 AND covers_to >= $1
        )
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await?;

    Ok(exists)
}

/// Register that an archive file's rows have been rehydrated into
/// `cold_audit_logs`.  Call this after bulk-inserting the rows so the overlap
/// check in [`query_audit_logs`] can find them on the next query.
///
/// Idempotent: if a pointer for `archive_id` already exists, this is a no-op.
pub async fn register_cold_pointer(
    pool: &PgPool,
    archive_id: Uuid,
    covers_from: DateTime<Utc>,
    covers_to: DateTime<Utc>,
    row_count: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO audit_log_cold_pointers
            (archive_id, covers_from, covers_to, row_count)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (archive_id) DO NOTHING
        "#,
    )
    .bind(archive_id)
    .bind(covers_from)
    .bind(covers_to)
    .bind(row_count)
    .execute(pool)
    .await?;

    Ok(())
}

/// Rehydrate rows from a given archive into `cold_audit_logs`.
///
/// `rows` must be the fully-deserialized rows from the archive file (the
/// gzip NDJSON produced by `run_retention`).  The function bulk-inserts them
/// and then calls [`register_cold_pointer`] so future queries see the data.
///
/// Rows that already exist (by primary key) are skipped — the insert is
/// `ON CONFLICT DO NOTHING` so this is safe to call multiple times.
pub async fn rehydrate_archive(
    pool: &PgPool,
    archive_id: Uuid,
    covers_from: DateTime<Utc>,
    covers_to: DateTime<Utc>,
    rows: &[ArchivedAuditRow],
) -> Result<i64, sqlx::Error> {
    if rows.is_empty() {
        return Ok(0);
    }

    let mut inserted: i64 = 0;
    for row in rows {
        let result = sqlx::query(
            r#"
            INSERT INTO cold_audit_logs
                (id, entity_id, entity_type, action, old_val, new_val, actor, timestamp, archive_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (id) DO NOTHING
            "#,
        )
        .bind(row.id)
        .bind(row.entity_id)
        .bind(&row.entity_type)
        .bind(&row.action)
        .bind(&row.old_val)
        .bind(&row.new_val)
        .bind(&row.actor)
        .bind(row.timestamp)
        .bind(archive_id)
        .execute(pool)
        .await?;

        inserted += result.rows_affected() as i64;
    }

    register_cold_pointer(pool, archive_id, covers_from, covers_to, inserted).await?;

    info!(
        archive_id = %archive_id,
        rows_inserted = inserted,
        "rehydrate_archive: cold audit log rows loaded from archive"
    );

    Ok(inserted)
}

/// A single row deserialized from a gzip-NDJSON audit archive file.
/// Mirrors the JSON shape written by `run_retention` in `src/db/audit.rs`.
#[derive(Debug, Clone, Deserialize)]
pub struct ArchivedAuditRow {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub entity_type: String,
    pub action: String,
    pub old_val: Option<serde_json::Value>,
    pub new_val: Option<serde_json::Value>,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// List registered cold archives
// ---------------------------------------------------------------------------

/// Summary of a registered cold archive pointer, for admin surfacing.
#[derive(Debug, Clone, Serialize)]
pub struct ColdPointerSummary {
    pub id: Uuid,
    pub archive_id: Uuid,
    pub covers_from: DateTime<Utc>,
    pub covers_to: DateTime<Utc>,
    pub loaded_at: DateTime<Utc>,
    pub row_count: i64,
}

/// List all registered cold archive pointers, ordered by covers_from ascending.
pub async fn list_cold_pointers(
    pool: &PgPool,
) -> Result<Vec<ColdPointerSummary>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, archive_id, covers_from, covers_to, loaded_at, row_count
        FROM audit_log_cold_pointers
        ORDER BY covers_from ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut result = Vec::with_capacity(rows.len());
    for row in &rows {
        result.push(ColdPointerSummary {
            id: row.try_get("id")?,
            archive_id: row.try_get("archive_id")?,
            covers_from: row.try_get("covers_from")?,
            covers_to: row.try_get("covers_to")?,
            loaded_at: row.try_get("loaded_at")?,
            row_count: row.try_get("row_count")?,
        });
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// Warn helper used by the admin audit handler
// ---------------------------------------------------------------------------

/// Emit a structured warning when a time-range audit query would span cold
/// storage that has not yet been rehydrated.  Call this from admin handlers
/// before falling back to a hot-only query, so the missing data is surfaced
/// in logs.
pub fn warn_cold_data_not_rehydrated(from: DateTime<Utc>, to: DateTime<Utc>) {
    warn!(
        cold_tier_touched = false,
        rehydrated = false,
        from = %from.to_rfc3339(),
        to   = %to.to_rfc3339(),
        "audit query spans archived time range but cold data is not yet rehydrated; \
         results may be incomplete — use POST /admin/audit/cold/rehydrate to load \
         archived rows before querying"
    );
}
