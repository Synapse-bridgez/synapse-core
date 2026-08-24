//! Part A regression test: `db::queries::insert_transaction` (a real write
//! path) must invalidate the *shared* `QueryCache` instance passed to it —
//! not a throwaway instance that never reaches whatever process actually
//! reads through the shared one. Before this fix,
//! `invalidate_transaction_caches` unconditionally constructed its own
//! `QueryCache::new(&redis_url)` and invalidated that, so entries in the
//! shared instance's in-memory LRU were never cleared by any write.
//!
//! This test builds one shared `QueryCache` (standing in for
//! `AppState.query_cache`), pre-populates its in-memory LRU directly via
//! `set`, then drives a real transaction insert through
//! `queries::insert_transaction` passing that *same* instance, and confirms
//! the pre-populated entry is gone when read back through that same instance.

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::time::Duration;
use synapse_core::db::queries;
use synapse_core::services::QueryCache;
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

#[ignore = "Requires Docker + Redis"]
#[tokio::test]
async fn test_insert_transaction_invalidates_shared_cache_instance() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let cache = match QueryCache::new(&redis_url).await {
        Ok(c) => c,
        Err(_) => {
            println!("Skipping: Redis not available");
            return;
        }
    };
    let (pool, _container) = setup_test_db().await;

    let asset_code = format!("TST{}", &uuid::Uuid::new_v4().simple().to_string()[..5]);

    // Pre-populate the shared instance's in-memory LRU directly, standing in
    // for a prior read that warmed the cache (e.g. a stats handler).
    cache
        .set(
            "query:status_counts",
            &serde_json::json!({"stale": true}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();
    cache
        .set(
            &format!("query:asset_total:{asset_code}"),
            &serde_json::json!({"stale": true}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    // Sanity: both reads currently hit the pre-populated (stale) entries.
    let pre: Option<serde_json::Value> = cache.get("query:status_counts").await.unwrap();
    assert!(
        pre.is_some(),
        "precondition: cache should be warm before insert"
    );

    // Drive a real write path, passing the *same* shared instance — this is
    // the fix: previously this call constructed its own throwaway QueryCache
    // internally and the line below would still have observed `pre`'s value.
    let tx = TransactionFixture::new()
        .with_asset_code(&asset_code)
        .build();
    queries::insert_transaction(&pool, &tx, Some(&cache))
        .await
        .expect("insert should succeed");

    // Re-read through the *same* shared instance: both the wildcard-matched
    // key and the exact per-asset key must now be gone.
    let status_counts: Option<serde_json::Value> = cache.get("query:status_counts").await.unwrap();
    let asset_total: Option<serde_json::Value> = cache
        .get(&format!("query:asset_total:{asset_code}"))
        .await
        .unwrap();
    assert!(
        status_counts.is_none(),
        "status_counts should be invalidated on the shared instance after insert"
    );
    assert!(
        asset_total.is_none(),
        "per-asset total should be invalidated on the shared instance after insert"
    );
}
