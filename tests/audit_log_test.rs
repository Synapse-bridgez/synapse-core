use serde_json::json;
use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::db::{
    audit::{AuditLog, ENTITY_TRANSACTION},
    queries::insert_transaction,
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

#[path = "fixtures.rs"]
mod fixtures;
use fixtures::TransactionFixture;

async fn setup_test_db() -> (PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        host_port
    );

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    // Create partition for current month
    let _ = sqlx::query(
        r#"
        DO $$
        DECLARE
            partition_date DATE;
            partition_name TEXT;
            start_date TEXT;
            end_date TEXT;
        BEGIN
            partition_date := DATE_TRUNC('month', NOW());
            partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
            start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
            end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');
            
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    partition_name, start_date, end_date
                );
            END IF;
        END $$;
        "#
    )
    .execute(&pool)
    .await;

    (pool, container)
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_on_insert() {
    let (pool, _container) = setup_test_db().await;

    let tx = TransactionFixture::new()
        .with_stellar_account("GTEST123")
        .with_amount("100.50")
        .with_callback_type("deposit")
        .with_callback_status("pending")
        .with_anchor_transaction_id("anchor-123")
        .build();
    let tx_id = tx.id;

    insert_transaction(&pool, &tx, None).await.unwrap();

    // Verify audit log was created
    let audit_log = sqlx::query(
        "SELECT entity_id, entity_type, action, new_val, actor FROM audit_logs WHERE entity_id = $1"
    )
    .bind(tx_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(audit_log.get::<Uuid, _>("entity_id"), tx_id);
    assert_eq!(
        audit_log.get::<String, _>("entity_type"),
        ENTITY_TRANSACTION
    );
    assert_eq!(audit_log.get::<String, _>("action"), "created");
    assert_eq!(audit_log.get::<String, _>("actor"), "system");

    let new_val: serde_json::Value = audit_log.get("new_val");
    assert_eq!(new_val["stellar_account"], "GTEST123");
    assert_eq!(new_val["status"], "pending");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_on_status_change() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let mut db_tx = pool.begin().await.unwrap();

    // Log status change
    AuditLog::log_status_change(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        "pending",
        "completed",
        "admin",
    )
    .await
    .unwrap();

    db_tx.commit().await.unwrap();

    // Verify audit log
    let audit_log =
        sqlx::query("SELECT action, old_val, new_val, actor FROM audit_logs WHERE entity_id = $1")
            .bind(tx_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(audit_log.get::<String, _>("action"), "status_update");
    assert_eq!(audit_log.get::<String, _>("actor"), "admin");

    let old_val: serde_json::Value = audit_log.get("old_val");
    let new_val: serde_json::Value = audit_log.get("new_val");
    assert_eq!(old_val["status"], "pending");
    assert_eq!(new_val["status"], "completed");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_on_field_update() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let settlement_id = Uuid::new_v4();
    let mut db_tx = pool.begin().await.unwrap();

    // Log field update
    AuditLog::log_field_update(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        "settlement_id",
        json!(null),
        json!(settlement_id.to_string()),
        "system",
    )
    .await
    .unwrap();

    db_tx.commit().await.unwrap();

    // Verify audit log
    let audit_log =
        sqlx::query("SELECT action, old_val, new_val FROM audit_logs WHERE entity_id = $1")
            .bind(tx_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(audit_log.get::<String, _>("action"), "settlement_id_update");

    let old_val: serde_json::Value = audit_log.get("old_val");
    let new_val: serde_json::Value = audit_log.get("new_val");
    assert!(old_val["settlement_id"].is_null());
    assert_eq!(new_val["settlement_id"], settlement_id.to_string());
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_on_deletion() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let mut db_tx = pool.begin().await.unwrap();

    // Log deletion
    AuditLog::log_deletion(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        json!({
            "stellar_account": "GTEST123",
            "amount": "100.50",
            "status": "completed"
        }),
        "admin",
    )
    .await
    .unwrap();

    db_tx.commit().await.unwrap();

    // Verify audit log
    let audit_log =
        sqlx::query("SELECT action, old_val, new_val, actor FROM audit_logs WHERE entity_id = $1")
            .bind(tx_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(audit_log.get::<String, _>("action"), "deleted");
    assert_eq!(audit_log.get::<String, _>("actor"), "admin");

    let old_val: serde_json::Value = audit_log.get("old_val");
    let new_val: Option<serde_json::Value> = audit_log.get("new_val");
    assert_eq!(old_val["stellar_account"], "GTEST123");
    assert_eq!(old_val["status"], "completed");
    assert!(new_val.is_none());
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_query() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let mut db_tx = pool.begin().await.unwrap();

    // Create multiple audit logs
    AuditLog::log_creation(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        json!({"status": "pending"}),
        "system",
    )
    .await
    .unwrap();

    AuditLog::log_status_change(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        "pending",
        "processing",
        "system",
    )
    .await
    .unwrap();

    AuditLog::log_status_change(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        "processing",
        "completed",
        "admin",
    )
    .await
    .unwrap();

    db_tx.commit().await.unwrap();

    // Query all logs for this entity
    let logs = sqlx::query(
        "SELECT action, actor FROM audit_logs WHERE entity_id = $1 ORDER BY timestamp ASC",
    )
    .bind(tx_id)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(logs.len(), 3);
    assert_eq!(logs[0].get::<String, _>("action"), "created");
    assert_eq!(logs[1].get::<String, _>("action"), "status_update");
    assert_eq!(logs[2].get::<String, _>("action"), "status_update");
    assert_eq!(logs[2].get::<String, _>("actor"), "admin");

    // Query by entity_type
    let type_logs = sqlx::query("SELECT COUNT(*) as count FROM audit_logs WHERE entity_type = $1")
        .bind(ENTITY_TRANSACTION)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(type_logs.get::<i64, _>("count"), 3);

    // Query by actor
    let actor_logs =
        sqlx::query("SELECT COUNT(*) as count FROM audit_logs WHERE entity_id = $1 AND actor = $2")
            .bind(tx_id)
            .bind("admin")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(actor_logs.get::<i64, _>("count"), 1);
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_audit_log_immutability() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let mut db_tx = pool.begin().await.unwrap();

    // Create audit log
    AuditLog::log_creation(
        &mut db_tx,
        tx_id,
        ENTITY_TRANSACTION,
        json!({"status": "pending"}),
        "system",
    )
    .await
    .unwrap();

    db_tx.commit().await.unwrap();

    // Get the audit log ID
    let audit_log = sqlx::query("SELECT id, action FROM audit_logs WHERE entity_id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let audit_id: Uuid = audit_log.get("id");
    let original_action: String = audit_log.get("action");

    // Attempt to update the audit log (should succeed but violates compliance)
    let update_result = sqlx::query("UPDATE audit_logs SET action = $1 WHERE id = $2")
        .bind("modified")
        .bind(audit_id)
        .execute(&pool)
        .await;

    // Verify update succeeded (no DB constraint prevents it)
    assert!(update_result.is_ok());

    // Verify the action was changed (demonstrating lack of immutability at DB level)
    let updated_log = sqlx::query("SELECT action FROM audit_logs WHERE id = $1")
        .bind(audit_id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let updated_action: String = updated_log.get("action");
    assert_ne!(updated_action, original_action);
    assert_eq!(updated_action, "modified");

    // Note: This test demonstrates that audit logs are NOT immutable at the database level.
    // For true immutability, consider:
    // 1. Database-level triggers to prevent UPDATE/DELETE
    // 2. Append-only table with no UPDATE permissions
    // 3. Blockchain or cryptographic verification
}

// ---------------------------------------------------------------------------
// Part F regression tests: retention must not delete rows unless the
// archive write is confirmed durable.
// ---------------------------------------------------------------------------

/// Test-only `ArchiveStorage` that always fails, standing in for a durable
/// backend being unreachable (network partition, bucket permissions, etc.).
struct FailingArchiveStorage;

#[async_trait::async_trait]
impl synapse_core::db::audit::ArchiveStorage for FailingArchiveStorage {
    async fn write(
        &self,
        _key: &str,
        _data: &[u8],
    ) -> Result<
        synapse_core::db::audit::ArchiveWriteConfirmation,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        Err("simulated durable-storage outage".into())
    }

    fn location_description(&self, key: &str) -> String {
        format!("unreachable-backend/{key}")
    }
}

async fn insert_old_audit_log(pool: &PgPool, entity_id: Uuid) {
    sqlx::query(
        "INSERT INTO audit_logs (entity_id, entity_type, action, actor, timestamp) \
         VALUES ($1, $2, $3, $4, NOW() - INTERVAL '400 days')",
    )
    .bind(entity_id)
    .bind(ENTITY_TRANSACTION)
    .bind("status_update")
    .bind("system")
    .execute(pool)
    .await
    .unwrap();
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn retention_skips_deletion_when_archive_write_fails() {
    use synapse_core::db::audit::run_retention;

    let (pool, _container) = setup_test_db().await;
    let entity_id = Uuid::new_v4();
    insert_old_audit_log(&pool, entity_id).await;

    let cutoff = chrono::Utc::now() - chrono::Duration::days(365);
    let storage = FailingArchiveStorage;

    let result = run_retention(&pool, cutoff, &storage).await;
    assert!(
        result.is_err(),
        "run_retention must return Err when the archive write fails"
    );

    // The row must still be present — the hard invariant this fixes.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1")
        .bind(entity_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "row must NOT be deleted when the archive write failed"
    );

    // No archive metadata should have been recorded either.
    let archive_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log_archives")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        archive_count, 0,
        "no archive metadata row should exist for a failed write"
    );
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn retention_deletes_rows_and_records_archive_metadata_on_success() {
    use synapse_core::db::audit::{run_retention, LocalDiskArchiveStorage};

    let (pool, _container) = setup_test_db().await;
    let entity_id = Uuid::new_v4();
    insert_old_audit_log(&pool, entity_id).await;

    let cutoff = chrono::Utc::now() - chrono::Duration::days(365);
    let temp_dir = tempfile::tempdir().unwrap();
    let storage = LocalDiskArchiveStorage::new(temp_dir.path().to_string_lossy().to_string());

    let result = run_retention(&pool, cutoff, &storage)
        .await
        .expect("retention run should succeed")
        .expect("there should be work to do");
    assert_eq!(result.exported, 1);
    assert_eq!(result.deleted, 1);

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1")
        .bind(entity_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 0,
        "row should be deleted after a successful archive write"
    );

    let archive_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log_archives WHERE location = $1")
            .bind(&result.archive_path)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        archive_count, 1,
        "a metadata row should be recorded for the successful archive"
    );
}
