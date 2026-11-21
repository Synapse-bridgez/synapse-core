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
// Query builder
// ---------------------------------------------------------------------------

/// Optional filters for querying audit logs.
///
/// `from` and `to` are inclusive bounds on `audit_logs.timestamp`; when both
/// are `None` the full range is returned.
#[derive(Debug, Clone, Default)]
pub struct AuditLogFilter {
    pub entity_id: Option<Uuid>,
    pub entity_type: Option<String>,
    pub action: Option<String>,
    pub actor: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: i64,
    pub offset: i64,
}

/// Query audit logs applying the supplied filter, newest first.
pub async fn query_audit_logs(
    pool: &PgPool,
    filter: &AuditLogFilter,
) -> sqlx::Result<Vec<AuditLog>> {
    let rows = sqlx::query(
        r#"
        SELECT entity_id, entity_type, action, old_val, new_val, actor, timestamp
        FROM audit_logs
        WHERE ($1::uuid IS NULL OR entity_id = $1)
          AND ($2::text IS NULL OR entity_type = $2)
          AND ($3::text IS NULL OR action = $3)
          AND ($4::text IS NULL OR actor = $4)
          AND ($5::timestamptz IS NULL OR timestamp >= $5)
          AND ($6::timestamptz IS NULL OR timestamp <= $6)
        ORDER BY timestamp DESC
        LIMIT $7 OFFSET $8
        "#,
    )
    .bind(filter.entity_id)
    .bind(filter.entity_type.as_deref())
    .bind(filter.action.as_deref())
    .bind(filter.actor.as_deref())
    .bind(filter.from)
    .bind(filter.to)
    .bind(filter.limit)
    .bind(filter.offset)
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

/// Export audit logs older than `cutoff` (excluding protected entity IDs) to a
/// gzip-compressed NDJSON archive on `storage`, record the archive in
/// `audit_log_archives`, then delete them from `audit_logs`.
///
/// Rows whose `entity_id` belongs to a transaction with status `'disputed'` are
/// never deleted (they are still exported to the archive).
///
/// # Hard invariant
///
/// Rows are deleted **only if** `storage.write` returns `Ok`
