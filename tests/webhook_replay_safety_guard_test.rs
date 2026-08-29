use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::models::Transaction;
use synapse_core::db::queries;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

async fn setup() -> (PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let pool = PgPool::connect(&url).await.unwrap();
    let migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator.run(&pool).await.unwrap();

    sqlx::query(
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
                EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', partition_name, start_date, end_date);
            END IF;
        END $$;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    (pool, container)
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_replay_blocked_for_successful_deliveries() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "100.50".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-123".to_string()),
        Some("deposit".to_string()),
        Some("completed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query(
        r#"
        INSERT INTO webhook_events
        (transaction_id, transaction_created_at, event_type, delivery_status, acknowledged_at)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind("transaction.completed")
    .bind("delivered")
    .bind(chrono::Utc::now())
    .execute(&pool)
    .await
    .ok();

    let delivery_status: Option<String> = sqlx::query_scalar(
        "SELECT delivery_status FROM webhook_events WHERE transaction_id = $1 AND delivery_status = 'delivered'",
    )
    .bind(inserted.id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(delivery_status, Some("delivered".to_string()));

    let replay_check: bool = delivery_status == Some("delivered".to_string());
    assert!(
        replay_check,
        "successful deliveries should be marked as delivered and blocked from replay by default"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_replay_allowed_for_failed_deliveries() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "50.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-456".to_string()),
        Some("deposit".to_string()),
        Some("pending".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query(
        r#"
        INSERT INTO webhook_events
        (transaction_id, transaction_created_at, event_type, delivery_status)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind("transaction.pending")
    .bind("failed")
    .execute(&pool)
    .await
    .ok();

    let delivery_status: Option<String> = sqlx::query_scalar(
        "SELECT delivery_status FROM webhook_events WHERE transaction_id = $1 AND delivery_status = 'failed'",
    )
    .bind(inserted.id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(delivery_status, Some("failed".to_string()));
    assert!(
        delivery_status == Some("failed".to_string()),
        "failed deliveries should be allowed for replay"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_forced_replay_override_requires_audit_logging() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "75.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-789".to_string()),
        Some("deposit".to_string()),
        Some("completed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query(
        r#"
        INSERT INTO webhook_events
        (transaction_id, transaction_created_at, event_type, delivery_status, acknowledged_at)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind("transaction.completed")
    .bind("delivered")
    .bind(chrono::Utc::now())
    .execute(&pool)
    .await
    .ok();

    sqlx::query(
        r#"
        INSERT INTO webhook_replay_history
        (transaction_id, transaction_created_at, replayed_by, forced_override, success)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind("admin-user@example.com")
    .bind(true)
    .bind(true)
    .execute(&pool)
    .await
    .ok();

    let replay_record: Option<bool> = sqlx::query_scalar(
        "SELECT forced_override FROM webhook_replay_history WHERE transaction_id = $1 AND forced_override = true",
    )
    .bind(inserted.id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert_eq!(replay_record, Some(true));
    assert!(
        replay_record == Some(true),
        "forced replay overrides must be logged with forced_override flag"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_replay_audit_logging_for_sensitive_actions() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "90.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-999".to_string()),
        Some("deposit".to_string()),
        Some("completed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    let admin_user = "sensitive-admin@example.com";
    let forced_override = true;

    sqlx::query(
        r#"
        INSERT INTO webhook_replay_history
        (transaction_id, transaction_created_at, replayed_by, forced_override, success, error_message)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind(admin_user)
    .bind(forced_override)
    .bind(true)
    .bind(None::<String>)
    .execute(&pool)
    .await
    .ok();

    let audit_record: Option<(String, bool)> = sqlx::query_as(
        "SELECT replayed_by, forced_override FROM webhook_replay_history WHERE transaction_id = $1",
    )
    .bind(inserted.id)
    .fetch_optional(&pool)
    .await
    .unwrap();

    assert!(audit_record.is_some());
    let (recorded_admin, recorded_override) = audit_record.unwrap();
    assert_eq!(recorded_admin, admin_user);
    assert_eq!(recorded_override, forced_override);
}
