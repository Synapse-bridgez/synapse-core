//! Verifies that `db::queries::insert_transaction` is safe to retry.
//!
//! `retry_with_backoff` may re-invoke the insert closure after a transient
//! error that occurs *after* the underlying INSERT already committed (e.g.
//! the connection drops while the client is reading the commit
//! acknowledgement). This test simulates that scenario by calling
//! `insert_transaction` twice with the same `Transaction` (same id +
//! created_at) and asserts the retry observes the already-committed row
//! instead of inserting a duplicate or writing a second audit log entry.

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries::insert_transaction;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

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
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    partition_name, start_date, end_date
                );
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
async fn test_retried_insert_does_not_duplicate_row_or_audit_log() {
    let (pool, _container) = setup_test_db().await;

    let tx = TransactionFixture::pending_deposit();

    let (first, first_is_new) = insert_transaction(&pool, &tx)
        .await
        .expect("first insert should succeed");
    assert_eq!(first.id, tx.id);
    assert!(first_is_new, "first insert should create a fresh row");

    // Simulate a retry: the same caller-generated `tx` (same id + created_at)
    // is inserted again, as `retry_with_backoff` would do after a transient
    // post-commit error on the first attempt.
    let (retried, retried_is_new) = insert_transaction(&pool, &tx)
        .await
        .expect("retried insert should be idempotent, not error");
    assert_eq!(retried.id, tx.id);
    assert_eq!(retried.stellar_account, first.stellar_account);
    assert!(
        !retried_is_new,
        "retry should observe the already-committed row, not create a new one"
    );

    let row_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE id = $1")
        .bind(tx.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row_count, 1, "retry must not duplicate the committed row");

    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1 AND entity_type = 'transaction'",
    )
    .bind(tx.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        audit_count, 1,
        "retry must not write a second audit log entry for the same insert"
    );
}
