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

// ── #1115: partition-rotation-during-cache-lifetime ──────────────────────────

/// Verify that creating a new partition via
/// `create_month_partition_and_invalidate_cache` invalidates only the
/// transaction-aggregate cache keys and leaves unrelated keys intact.
///
/// # Correctness being tested
///
/// Before this fix, `create_month_partition` (via `cron.rs`) had no cache
/// awareness; any cached aggregate result for `transactions` would remain
/// valid until its Redis TTL expired, silently serving stale data for up to
/// `status_counts_ttl` seconds (default 300 s) after a new partition was
/// attached. This test proves that the wrapper invalidates the right keys.
#[ignore = "Requires Docker + Redis"]
#[tokio::test]
async fn test_partition_rotation_invalidates_transaction_cache_keys() {
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

    // Pre-populate transaction-aggregate keys (simulating warm cache before rotation).
    cache
        .set(
            "query:status_counts",
            &serde_json::json!({"pending": 5}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();
    cache
        .set(
            "query:daily_totals:7",
            &serde_json::json!({"total": 1000}),
            Duration::from_secs(3600),
        )
        .await
        .unwrap();
    cache
        .set(
            "query:asset_stats",
            &serde_json::json!({"USD": 500}),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
    cache
        .set(
            "query:asset_total:USD",
            &serde_json::json!(500),
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    // Also populate a non-transaction key that must NOT be evicted.
    cache
        .set(
            "query:settlement_stats",
            &serde_json::json!({"count": 3}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    // Sanity: keys are warm.
    let pre: Option<serde_json::Value> = cache.get("query:status_counts").await.unwrap();
    assert!(pre.is_some(), "precondition: status_counts must be warm");

    // Trigger partition rotation (use a far-future month to avoid conflicts).
    synapse_core::db::cron::create_month_partition_and_invalidate_cache(
        &pool,
        2099,
        1,
        Some(&cache),
    )
    .await
    .expect("partition creation must succeed");

    // Transaction-aggregate keys must be gone.
    let status: Option<serde_json::Value> = cache.get("query:status_counts").await.unwrap();
    let daily: Option<serde_json::Value> = cache.get("query:daily_totals:7").await.unwrap();
    let asset_stats: Option<serde_json::Value> = cache.get("query:asset_stats").await.unwrap();
    let asset_total: Option<serde_json::Value> =
        cache.get("query:asset_total:USD").await.unwrap();

    assert!(
        status.is_none(),
        "query:status_counts must be invalidated after partition creation"
    );
    assert!(
        daily.is_none(),
        "query:daily_totals:7 must be invalidated after partition creation"
    );
    assert!(
        asset_stats.is_none(),
        "query:asset_stats must be invalidated after partition creation"
    );
    assert!(
        asset_total.is_none(),
        "query:asset_total:USD must be invalidated after partition creation"
    );

    // Non-transaction key must be untouched (no over-invalidation).
    let settlement_stats: Option<serde_json::Value> =
        cache.get("query:settlement_stats").await.unwrap();
    assert!(
        settlement_stats.is_some(),
        "query:settlement_stats must NOT be invalidated by a partition rotation event — \
         only transaction-aggregate keys should be affected"
    );
}

/// Verify that detaching old partitions via
/// `detach_and_archive_old_partitions_and_invalidate_cache` similarly
/// invalidates transaction-aggregate keys without over-invalidating.
#[ignore = "Requires Docker + Redis"]
#[tokio::test]
async fn test_partition_detach_invalidates_transaction_cache_keys() {
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

    // Create a very old partition so the detach actually finds something to do.
    synapse_core::db::cron::create_month_partition(&pool, 2020, 1)
        .await
        .expect("partition creation for detach test must succeed");

    cache
        .set(
            "query:status_counts",
            &serde_json::json!({"pending": 2}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();
    cache
        .set(
            "query:settlement_stats",
            &serde_json::json!({"count": 1}),
            Duration::from_secs(300),
        )
        .await
        .unwrap();

    // Detach partitions older than 12 months — will detach the 2020-01 one.
    synapse_core::db::cron::detach_and_archive_old_partitions_and_invalidate_cache(
        &pool,
        12,
        Some(&cache),
    )
    .await
    .expect("detach must succeed");

    let status: Option<serde_json::Value> = cache.get("query:status_counts").await.unwrap();
    assert!(
        status.is_none(),
        "query:status_counts must be invalidated after partition detach"
    );

    let settlement_stats: Option<serde_json::Value> =
        cache.get("query:settlement_stats").await.unwrap();
    assert!(
        settlement_stats.is_some(),
        "query:settlement_stats must NOT be invalidated by a partition detach event"
    );
}
