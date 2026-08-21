use bigdecimal::BigDecimal;
use sqlx::migrate::Migrator;
use sqlx::PgPool;
use std::path::Path;
use std::str::FromStr;
use synapse_core::db::models::Transaction;
use synapse_core::services::TransactionProcessor;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

async fn setup_db() -> (PgPool, ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let pool = PgPool::connect(&url).await.unwrap();

    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();

    sqlx::query(
        r#"
        DO $$
        DECLARE
            p_date DATE := DATE_TRUNC('month', NOW());
            p_name TEXT := 'transactions_y' || TO_CHAR(p_date, 'YYYY') || 'm' || TO_CHAR(p_date, 'MM');
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = p_name) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    p_name,
                    TO_CHAR(p_date, 'YYYY-MM-DD'),
                    TO_CHAR(p_date + INTERVAL '1 month', 'YYYY-MM-DD')
                );
            END IF;
        END $$;
        "#,
    )
    .execute(&pool)
    .await
    .expect("Failed to ensure current month partition");

    (pool, container)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_dlq_workflow() {
    let (pool, _container) = setup_db().await;

    let tx_id = uuid::Uuid::new_v4();
    let amount = BigDecimal::from_str("100.50").unwrap();

    sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("pending")
    .execute(&pool)
    .await
    .expect("Failed to insert test transaction");

    let processor = TransactionProcessor::new(pool.clone());
    let result = processor.process_transaction(tx_id).await;
    assert!(result.is_ok(), "Transaction processing should succeed");

    let tx = sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to fetch transaction");

    assert_eq!(tx.status, "completed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_requeue_dlq() {
    let (pool, _container) = setup_db().await;

    let tx_id = uuid::Uuid::new_v4();
    let amount = BigDecimal::from_str("100.50").unwrap();

    sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("dlq")
    .execute(&pool)
    .await
    .expect("Failed to insert test transaction");

    let dlq_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO transaction_dlq (id, transaction_id, stellar_account, amount, asset_code, error_reason, retry_count, original_created_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())"#,
    )
    .bind(dlq_id)
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("Test error")
    .bind(3)
    .execute(&pool)
    .await
    .expect("Failed to insert DLQ entry");

    let processor = TransactionProcessor::new(pool.clone());
    let result = processor.requeue_dlq(dlq_id).await;
    assert!(result.is_ok(), "Requeue should succeed");

    let tx = sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to fetch transaction");

    assert_eq!(tx.status, "pending");

    let dlq_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transaction_dlq WHERE id = $1")
        .bind(dlq_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to count DLQ entries");

    assert_eq!(dlq_count, 0, "DLQ entry should be removed");
}
