use chrono::{DateTime, Utc};
use flate2::{write::GzEncoder, Compression};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction as SqlxTransaction};
use std::io::Write;
use uuid::Uuid;

/// Entity type constants for audit logs
pub const ENTITY_TRANSACTION: &str = "transaction";
pub const ENTITY_SETTLEMENT: &str = "settlement";

/// Represents an audit log entry
#[derive(Debug, Clone)]
pub struct AuditLog {
    pub entity_id: Uuid,
    pub entity_type: String,
    pub action: String,
    pub old_val: Option<JsonValue>,
    pub new_val: Option<JsonValue>,
    pub actor: String,
    pub timestamp: DateTime<Utc>,
}

impl AuditLog {
    /// Create a new audit log entry
    pub fn new(
        entity_id: Uuid,
        entity_type: impl Into<String>,
        action: impl Into<String>,
        old_val: Option<JsonValue>,
        new_val: Option<JsonValue>,
        actor: impl Into<String>,
    ) -> Self {
        Self {
            entity_id,
            entity_type: entity_type.into(),
            action: action.into(),
            old_val,
            new_val,
            actor: actor.into(),
            timestamp: Utc::now(),
        }
    }

    /// Log an action with explicit old and new values
    pub async fn log(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        action: &str,
        old_val: Option<JsonValue>,
        new_val: Option<JsonValue>,
        actor: &str,
    ) -> sqlx::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO audit_logs (entity_id, entity_type, action, old_val, new_val, actor)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(entity_id)
        .bind(entity_type)
        .bind(action)
        .bind(old_val)
        .bind(new_val)
        .bind(actor)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    /// Log a status change
    pub async fn log_status_change(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        old_status: &str,
        new_status: &str,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "status_update",
            Some(json!({ "status": old_status })),
            Some(json!({ "status": new_status })),
            actor,
        )
        .await
    }

    /// Log a field update
    pub async fn log_field_update(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        field_name: &str,
        old_value: JsonValue,
        new_value: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            &format!("{field_name}_update"),
            Some(json!({ field_name: old_value })),
            Some(json!({ field_name: new_value })),
            actor,
        )
        .await
    }

    /// Log a creation event
    pub async fn log_creation(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        created_data: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "created",
            None,
            Some(created_data),
            actor,
        )
        .await
    }

    /// Log a deletion event
    pub async fn log_deletion(
        tx: &mut SqlxTransaction<'_, Postgres>,
        entity_id: Uuid,
        entity_type: &str,
        deleted_data: JsonValue,
        actor: &str,
    ) -> sqlx::Result<()> {
        Self::log(
            tx,
            entity_id,
            entity_type,
            "deleted",
            Some(deleted_data),
            None,
            actor,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Paginated listing
// ---------------------------------------------------------------------------

/// SQL used to page through `audit_logs`. Pagination is pushed into the
/// database via `LIMIT`/`OFFSET` so large audit tables are never read end to
/// end. Kept as a constant so tests can assert the generated query contains
/// `LIMIT`.
pub const PAGINATED_AUDIT_LOGS_QUERY: &str = r#"
    SELECT entity_id, entity_type, action, old_val, new_val, actor, timestamp
    FROM audit_logs
    ORDER BY timestamp DESC, entity_id
    LIMIT $1 OFFSET $2
"#;

/// Fetch a single page of audit log entries, ordered newest first.
///
/// `limit` and `offset` are applied in SQL; callers are responsible for
/// enforcing any page-size cap before calling this.
pub async fn list_audit_logs_paginated(
    pool: &PgPool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<Vec<AuditLog>> {
    let rows = sqlx::query(PAGINATED_AUDIT_LOGS_QUERY)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|row| AuditLog {
            entity_id: row.get("entity_id"),
            entity_type: row.get("entity_type"),
            action: row.get("action"),
            old_val: row.get("old_val"),
            new_val: row.get("new_val"),
            actor: row.get("actor"),
            timestamp: row.get("timestamp"),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Retention policy
// ---------------------------------------------------------------------------

/// Default retention period in days (1 year).
pub const DEFAULT_RETENTION_DAYS: i64 = 365;

/// Read the retention period from the `AUDIT_LOG_RETENTION_DAYS` env var,
/// falling back to [`DEFAULT_RETENTION_DAYS`].
pub fn retention_days() -> i64 {
    std::env::var("AUDIT_LOG_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|&d| d > 0)
        .unwrap_or(DEFAULT_RETENTION_DAYS)
}

/// Result of a single retention run.
#[derive(Debug, Clone)]
pub struct RetentionResult {
    /// Number of rows exported to the archive file.
    pub exported: usize,
    /// Location the gzip-compressed archive was written to (backend-specific:
    /// a local path for `LocalDiskArchiveStorage`, a bucket/key URI for a
    /// future durable backend).
    pub archive_path: String,
    /// Number of rows deleted from the database.
    pub deleted: u64,
}

/// Confirmation that an archive write landed on its storage backend.
#[derive(Debug, Clone)]
pub struct ArchiveWriteConfirmation {
    /// SHA-256 hex digest of the archived bytes, as actually written.
    pub checksum: String,
}

/// Pluggable durable-storage backend for audit log archives.
///
/// `run_retention`'s hard invariant is: rows are deleted from `audit_logs`
/// only after `write` returns `Ok`. Implementations must return `Err` rather
/// than `Ok` on any doubt about durability — a write that merely landed on
/// local disk (see `LocalDiskArchiveStorage`) is explicitly NOT what "durable"
/// means in a deployment with ephemeral containers; see
/// `docs/audit_log_retention.md`.
#[async_trait::async_trait]
pub trait ArchiveStorage: Send + Sync {
    async fn write(
        &self,
        key: &str,
        data: &[u8],
    ) -> Result<ArchiveWriteConfirmation, Box<dyn std::error::Error + Send + Sync>>;

    /// Human-readable location description for the given key, recorded in
    /// `audit_log_archives.location` (e.g. a filesystem path or bucket URI).
    fn location_description(&self, key: &str) -> String;
}

/// Writes archives to local disk. **Not durable** in any deployment where
/// application instances run in ephemeral containers: the archive file (and
/// with it, the only remaining copy of the deleted rows) is lost on the next
/// redeploy/restart/reschedule of whichever container happened to run this
/// job. This is the only backend implemented today; see
/// `docs/audit_log_retention.md` for why a real durable backend (object
/// storage, a dedicated archival database) is left as a follow-up rather
/// than guessed at here.
pub struct LocalDiskArchiveStorage {
    dir: String,
}

impl LocalDiskArchiveStorage {
    pub fn new(dir: impl Into<String>) -> Self {
        Self { dir: dir.into() }
    }
}

#[async_trait::async_trait]
impl ArchiveStorage for LocalDiskArchiveStorage {
    async fn write(
        &self,
        key: &str,
        data: &[u8],
    ) -> Result<ArchiveWriteConfirmation, Box<dyn std::error::Error + Send + Sync>> {
        std::fs::create_dir_all(&self.dir)?;
        let path = format!("{}/{}", self.dir, key);
        std::fs::write(&path, data)?;

        let mut hasher = Sha256::new();
        hasher.update(data);
        let checksum = format!("{:x}", hasher.finalize());

        Ok(ArchiveWriteConfirmation { checksum })
    }

    fn location_description(&self, key: &str) -> String {
        format!("{}/{}", self.dir, key)
    }
}

#[cfg(test)]
mod pagination_tests {
    use super::PAGINATED_AUDIT_LOGS_QUERY;

    #[test]
    fn paginated_query_uses_sql_limit_and_offset() {
        assert!(PAGINATED_AUDIT_LOGS_QUERY.contains("LIMIT"));
        assert!(PAGINATED_AUDIT_LOGS_QUERY.contains("OFFSET"));
    }
}
