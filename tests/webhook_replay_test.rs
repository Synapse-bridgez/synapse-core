use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::models::Transaction;
use synapse_core::db::queries;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

// This file used to be `#[sqlx::test]`-based, which needs the connecting
// role to `LOCK TABLE pg_catalog.pg_namespace` while provisioning its
// ephemeral per-test database — a catalog-level lock that, in practice,
// only a superuser can reliably take. That was never a problem while every
// environment connected as the Postgres bootstrap superuser (see the Part A
// fix this same change makes for the actual application traffic), but once
// CI's shared `DATABASE_URL` moved to the restricted, explicitly
// NOBYPASSRLS `synapse_app` role, `#[sqlx::test]` started failing with
// "permission denied for table pg_namespace" — a real, if narrow,
// consequence of that role restriction, unrelated to RLS itself. Converted
// to the same testcontainers-per-test pattern the rest of this suite
// already uses (fresh throwaway Postgres, connect as its own bootstrap
// superuser), which sidesteps the requirement entirely.
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
async fn test_webhook_replay_tracking() {
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

    // Simulate a replay attempt
    sqlx::query(
        r#"
        INSERT INTO webhook_replay_history
        (transaction_id, transaction_created_at, replayed_by, dry_run, success, error_message)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(inserted.id)
    .bind(inserted.created_at)
    .bind("test-admin")
    .bind(true)
    .bind(true)
    .bind(None::<String>)
    .execute(&pool)
    .await
    .unwrap();

    // Verify the replay was tracked
    let replay_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM webhook_replay_history WHERE transaction_id = $1")
            .bind(inserted.id)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(replay_count, 1);
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_list_failed_webhooks() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "50.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-456".to_string()),
        Some("deposit".to_string()),
        Some("failed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query("UPDATE transactions SET status = 'failed' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

    let failed_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE status = 'failed'")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert!(failed_count >= 1);
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_replay_updates_status() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "75.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-789".to_string()),
        Some("deposit".to_string()),
        Some("failed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query("UPDATE transactions SET status = 'failed' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

    // Simulate replay by updating status to pending
    sqlx::query("UPDATE transactions SET status = 'pending', updated_at = NOW() WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

    let updated_tx = queries::get_transaction(&pool, inserted.id).await.unwrap();
    assert_eq!(updated_tx.status, "pending");
}
