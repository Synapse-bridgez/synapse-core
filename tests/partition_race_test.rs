//! Part A regression test: a burst of concurrent inserts landing in a month
//! nobody has created a partition for yet must all self-heal successfully,
//! rather than some of them getting a duplicate-relation 500 from a race in
//! the partition-creation path.

use chrono::{TimeZone, Utc};
use futures::future::join_all;
use sqlx::{migrate::Migrator, PgPool, Row};
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

    (pool, container)
}

async fn partition_count_for(pool: &PgPool, partition_name: &str) -> i64 {
    let row = sqlx::query("SELECT COUNT(*) as cnt FROM pg_class WHERE relname = $1")
        .bind(partition_name)
        .fetch_one(pool)
        .await
        .unwrap();
    row.get("cnt")
}

/// Deliberately targets a month far enough in the future that no migration or
/// scheduled maintenance tick could plausibly have created its partition —
/// this is the "missing partition" precondition, produced without needing to
/// delete/skip any existing partition.
fn far_future_month() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2031, 6, 15, 12, 0, 0).unwrap()
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_concurrent_burst_insert_into_missing_partition_all_self_heal() {
    let (pool, _container) = setup_test_db().await;
    let target_created_at = far_future_month();
    let partition_name = "transactions_y2031m06";

    // Precondition: the target partition does not exist yet.
    assert_eq!(
        partition_count_for(&pool, partition_name).await,
        0,
        "test precondition violated: partition already exists"
    );

    const CONCURRENCY: usize = 20;
    let mut txs = Vec::with_capacity(CONCURRENCY);
    for i in 0..CONCURRENCY {
        let mut tx = TransactionFixture::new()
            .with_stellar_account(&format!("GBURST{:050}", i))
            .with_amount("10.00")
            .build();
        tx.created_at = target_created_at;
        tx.updated_at = target_created_at;
        txs.push(tx);
    }

    // Fire all inserts at once so every one of them races to self-heal the
    // same missing partition concurrently.
    let results = join_all(txs.iter().map(|tx| insert_transaction(&pool, tx, None))).await;

    let failures: Vec<_> = results
        .iter()
        .enumerate()
        .filter(|(_, r)| r.is_err())
        .collect();
    assert!(
        failures.is_empty(),
        "expected all {} concurrent inserts into a missing partition to self-heal, \
         but {} failed: {:?}",
        CONCURRENCY,
        failures.len(),
        failures
            .iter()
            .map(|(i, r)| (i, r.as_ref().unwrap_err().to_string()))
            .collect::<Vec<_>>()
    );

    // Self-heal must not have raced into creating the partition twice.
    assert_eq!(
        partition_count_for(&pool, partition_name).await,
        1,
        "expected exactly one partition to exist after the concurrent self-heal burst"
    );

    // Every row actually landed.
    let row = sqlx::query(&format!("SELECT COUNT(*) as cnt FROM \"{partition_name}\""))
        .fetch_one(&pool)
        .await
        .unwrap();
    let count: i64 = row.get("cnt");
    assert_eq!(count, CONCURRENCY as i64);
}
