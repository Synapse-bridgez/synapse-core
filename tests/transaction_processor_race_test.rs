//! Part D regression test: two concurrent completions of the same
//! transaction through TransactionProcessor must not both succeed —
//! CompleteStage's row lock + WHERE status guard must let exactly one win.

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries::insert_transaction;
use synapse_core::services::TransactionProcessor;
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

    (pool, container)
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_concurrent_completions_only_one_wins() {
    let (pool, _container) = setup_test_db().await;

    let tx = TransactionFixture::new()
        .with_stellar_account("GRACECOMPLETE")
        .with_status("pending")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx).await.unwrap();

    let processor_a = TransactionProcessor::new(pool.clone());
    let processor_b = TransactionProcessor::new(pool.clone());

    let (result_a, result_b) = tokio::join!(
        processor_a.process_transaction(tx_id),
        processor_b.process_transaction(tx_id)
    );

    let successes = [&result_a, &result_b].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        successes, 1,
        "expected exactly one of the two concurrent completions to succeed, \
         got a={:?} b={:?}",
        result_a, result_b
    );

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        status, "completed",
        "the winning completion should still have applied"
    );
}
