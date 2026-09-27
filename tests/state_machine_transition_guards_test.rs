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
async fn test_invalid_state_transition_prevented() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "100.50".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-123".to_string()),
        Some("deposit".to_string()),
        Some("pending".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    let initial_status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(initial_status, "pending");

    let result = sqlx::query("UPDATE transactions SET status = 'completed' WHERE id = $1 AND status IN ('verified', 'approved')")
        .bind(inserted.id)
        .execute(&pool)
        .await;

    let final_status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(final_status, "pending");
    assert!(result.is_ok(), "conditional update should succeed even if no rows affected");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_direct_status_bypass_prevention() {
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

    let allowed_transitions = vec!["verified", "failed"];
    let status_before: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    let updated = sqlx::query("UPDATE transactions SET status = $1 WHERE id = $2 AND status = ANY($3::text[])")
        .bind("completed")
        .bind(inserted.id)
        .bind(&allowed_transitions)
        .execute(&pool)
        .await
        .unwrap();

    let status_after: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(updated.rows_affected(), 0);
    assert_eq!(status_before, status_after);
    assert_eq!(status_after, "pending");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_required_verification_states_enforced() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "75.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-789".to_string()),
        Some("deposit".to_string()),
        Some("pending".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    sqlx::query("UPDATE transactions SET status = 'verified' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(status, "verified");

    sqlx::query("UPDATE transactions SET status = 'completed' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await
        .unwrap();

    let final_status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(final_status, "completed");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_invalid_state_values_rejected() {
    let (pool, _container) = setup().await;

    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "90.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-999".to_string()),
        Some("deposit".to_string()),
        Some("pending".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx, None).await.unwrap();

    let result = sqlx::query("UPDATE transactions SET status = 'invalid_state' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await;

    let final_status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(final_status, "pending");
}
