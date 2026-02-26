use chrono::Utc;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::db::{
    audit::{AuditLog, ENTITY_TRANSACTION},
    models::Transaction,
    queries::insert_transaction,
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

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

#[tokio::test]
async fn test_audit_log_on_insert() {
    let (pool, _container) = setup_test_db().await;

    let tx_id = Uuid::new_v4();
    let tx = Transaction {
        id: tx_id,
        stellar_account: "GTEST123".to_string(),
        amount: "100.50".parse().unwrap(),
        asset_code: "USD".to_string(),
        status: "pending".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        anchor_transaction_id: Some("anchor-123".to_string()),
        callback_type: Some("deposit".to_string()),
        callback_status: Some("pending".to_string()),
        settlement_id: None,
        memo: None,
        memo_type: None,
        metadata: None,
    };

    insert_transaction(&pool, &tx).await.unwrap();

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
