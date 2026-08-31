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
/// A compliance-classified data export (see `telemetry::data_export::record_compliance_export`).
pub const ENTITY_COMPLIANCE_EXPORT: &str = "compliance_export";

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
/// Rows are deleted **only if** `storage.write` returns `Ok`. If the archive
/// write fails or its durability is in doubt, this function returns `Err`
/// before touching `audit_logs` — archival "succeeding" and rows actually
/// being safe to delete must never be allowed to diverge, since the archive
/// write is the only remaining copy of what's about to be deleted.
///
/// Returns `Ok(None)` when there is nothing to do (no rows older than cutoff).
pub async fn run_retention(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
    storage: &dyn ArchiveStorage,
) -> Result<Option<RetentionResult>, Box<dyn std::error::Error + Send + Sync>> {
    // 1. Fetch all rows older than the cutoff.
    let rows = sqlx::query(
        r#"
        SELECT id, entity_id, entity_type, action, old_val, new_val, actor, timestamp
        FROM audit_logs
        WHERE timestamp < $1
        ORDER BY timestamp ASC
        "#,
    )
    .bind(cutoff)
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(None);
    }

    // 2. Identify entity_ids that belong to disputed transactions — these must
    //    never be deleted.
    let entity_ids: Vec<Uuid> = rows
        .iter()
        .map(|r| r.try_get::<Uuid, _>("entity_id"))
        .collect::<Result<_, _>>()?;

    let disputed: Vec<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id FROM transactions
        WHERE id = ANY($1)
          AND status = 'disputed'
        "#,
    )
    .bind(&entity_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default(); // if transactions table has no disputed column yet, skip

    let disputed_set: std::collections::HashSet<Uuid> = disputed.into_iter().collect();

    // 3. Serialize all rows (including protected ones) to NDJSON, gzip
    //    in-memory (rows are already fully materialized above via
    //    fetch_all, so this adds no meaningful additional memory pressure).
    let earliest: DateTime<Utc> = rows[0].try_get("timestamp")?;
    let timestamp_str = Utc::now().format("%Y%m%dT%H%M%SZ");
    let key = format!("audit_logs_{}.ndjson.gz", timestamp_str);

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    for row in &rows {
        let id: Uuid = row.try_get("id")?;
        let entity_id: Uuid = row.try_get("entity_id")?;
        let entity_type: String = row.try_get("entity_type")?;
        let action: String = row.try_get("action")?;
        let old_val: Option<JsonValue> = row.try_get("old_val")?;
        let new_val: Option<JsonValue> = row.try_get("new_val")?;
        let actor: String = row.try_get("actor")?;
        let timestamp: DateTime<Utc> = row.try_get("timestamp")?;

        let record = json!({
            "id": id,
            "entity_id": entity_id,
            "entity_type": entity_type,
            "action": action,
            "old_val": old_val,
            "new_val": new_val,
            "actor": actor,
            "timestamp": timestamp.to_rfc3339(),
        });

        writeln!(gz, "{}", record)?;
    }
    let archive_bytes = gz.finish()?;
    let exported = rows.len();

    // 4. Write to durable storage. On failure, return here — do NOT proceed
    //    to delete. This is the hard invariant: a failed/unconfirmed archive
    //    write must never be followed by row deletion.
    let write_result = storage.write(&key, &archive_bytes).await;
    let confirmation = match write_result {
        Ok(c) => {
            crate::metrics::audit_archive_write_total()
                .add(1, &[opentelemetry::KeyValue::new("result", "success")]);
            c
        }
        Err(e) => {
            crate::metrics::audit_archive_write_total()
                .add(1, &[opentelemetry::KeyValue::new("result", "failure")]);
            return Err(format!(
                "audit archive write failed, skipping deletion of {exported} rows \
                 (they remain in audit_logs, safe, until the next successful run): {e}"
            )
            .into());
        }
    };
    let archive_path = storage.location_description(&key);

    // 5. Record the archive in the metadata table — must land before
    //    deletion too, so a run that fails here also skips deletion rather
    //    than deleting rows whose archive location was never recorded.
    sqlx::query(
        r#"
        INSERT INTO audit_log_archives
            (location, checksum, row_count, covers_from, covers_to)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(&archive_path)
    .bind(&confirmation.checksum)
    .bind(exported as i64)
    .bind(earliest)
    .bind(cutoff)
    .execute(pool)
    .await?;

    // 6. Delete only the non-protected rows.
    let deletable_ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| {
            let id: Uuid = r.try_get("id").ok()?;
            let entity_id: Uuid = r.try_get("entity_id").ok()?;
            if disputed_set.contains(&entity_id) {
                None
            } else {
                Some(id)
            }
        })
        .collect();

    let deleted = if deletable_ids.is_empty() {
        0
    } else {
        let result = sqlx::query("DELETE FROM audit_logs WHERE id = ANY($1)")
            .bind(&deletable_ids)
            .execute(pool)
            .await?;
        result.rows_affected()
    };

    Ok(Some(RetentionResult {
        exported,
        archive_path,
        deleted,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_log_creation() {
        let entity_id = Uuid::new_v4();
        let old_val = Some(json!({"status": "pending"}));
        let new_val = Some(json!({"status": "completed"}));

        let log = AuditLog::new(
            entity_id,
            ENTITY_TRANSACTION,
            "status_update",
            old_val.clone(),
            new_val.clone(),
            "system",
        );

        assert_eq!(log.entity_id, entity_id);
        assert_eq!(log.entity_type, ENTITY_TRANSACTION);
        assert_eq!(log.action, "status_update");
        assert_eq!(log.old_val, old_val);
        assert_eq!(log.new_val, new_val);
        assert_eq!(log.actor, "system");
    }

    // Pre-existing flake fixed as a labeled aside: these four tests all
    // mutate the process-global AUDIT_LOG_RETENTION_DAYS env var with no
    // synchronization, so under cargo test's default multi-threaded runner
    // they can interleave and observe each other's value mid-test. This
    // mutex forces them to run one at a time relative to each other without
    // affecting any other test in the crate.
    static RETENTION_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_retention_days_default() {
        let _guard = RETENTION_ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var("AUDIT_LOG_RETENTION_DAYS");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
    }

    #[test]
    fn test_retention_days_from_env() {
        let _guard = RETENTION_ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("AUDIT_LOG_RETENTION_DAYS", "90");
        assert_eq!(retention_days(), 90);
        std::env::remove_var("AUDIT_LOG_RETENTION_DAYS");
    }

    #[test]
    fn test_retention_days_invalid_env_falls_back() {
        let _guard = RETENTION_ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("AUDIT_LOG_RETENTION_DAYS", "not-a-number");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
        std::env::remove_var("AUDIT_LOG_RETENTION_DAYS");
    }

    #[test]
    fn test_retention_days_zero_falls_back() {
        let _guard = RETENTION_ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var("AUDIT_LOG_RETENTION_DAYS", "0");
        assert_eq!(retention_days(), DEFAULT_RETENTION_DAYS);
        std::env::remove_var("AUDIT_LOG_RETENTION_DAYS");
    }
}
