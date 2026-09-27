use synapse_core::services::{CacheConfig, QueryCache};

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_query_cache_basic_operations() {
    let cache = QueryCache::new("redis://localhost:6379").await.unwrap();

    // Test set and get
    let test_data = vec!["test1".to_string(), "test2".to_string()];
    cache
        .set("test:key", &test_data, std::time::Duration::from_secs(60))
        .await
        .unwrap();

    let retrieved: Option<Vec<String>> = cache.get("test:key").await.unwrap();
    assert_eq!(retrieved, Some(test_data));

    // Test cache miss
    let missing: Option<Vec<String>> = cache.get("nonexistent:key").await.unwrap();
    assert_eq!(missing, None);

    // Cleanup
    cache.invalidate_exact("test:key").await.unwrap();
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_cache_metrics() {
    let cache = QueryCache::new("redis://localhost:6379").await.unwrap();

    // Initial metrics
    let metrics = cache.metrics();
    let initial_total = metrics.total;

    // Trigger a miss
    let _: Option<Vec<String>> = cache.get("nonexistent:key").await.unwrap();

    // Check metrics updated
    let metrics = cache.metrics();
    assert_eq!(metrics.total, initial_total + 1);
    assert!(metrics.misses > 0);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_hit_rate_report_tracks_per_query_type_under_mixed_workload() {
    let cache = QueryCache::new("redis://localhost:6379").await.unwrap();

    // status_counts: one set + one hit
    cache
        .set(
            "query:status_counts",
            &"v".to_string(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let _: Option<String> = cache.get("query:status_counts").await.unwrap();

    // daily_totals: two misses (distinct keys, never set)
    let _: Option<String> = cache.get("query:daily_totals:1").await.unwrap();
    let _: Option<String> = cache.get("query:daily_totals:2").await.unwrap();

    let report = cache.hit_rate_report();
    let status_counts = report
        .iter()
        .find(|r| r.query_type == "status_counts")
        .expect("status_counts should appear in the report");
    assert!(status_counts.hits >= 1);

    let daily_totals = report
        .iter()
        .find(|r| r.query_type == "daily_totals")
        .expect("daily_totals should appear in the report");
    assert_eq!(daily_totals.misses, 2);
    assert_eq!(daily_totals.hit_rate, 0.0);

    cache.invalidate_exact("query:status_counts").await.unwrap();
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_cache_invalidation() {
    let cache = QueryCache::new("redis://localhost:6379").await.unwrap();

    // Set multiple keys
    cache
        .set(
            "test:pattern:1",
            &"value1",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    cache
        .set(
            "test:pattern:2",
            &"value2",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();

    // Invalidate by pattern
    cache.invalidate("test:pattern:*").await.unwrap();

    // Verify keys are gone
    let result1: Option<String> = cache.get("test:pattern:1").await.unwrap();
    let result2: Option<String> = cache.get("test:pattern:2").await.unwrap();
    assert_eq!(result1, None);
    assert_eq!(result2, None);
}

/// Part A regression test: `CacheEntry.expires_at` (previously dead code —
/// see `#[allow(dead_code)]` this removes) must actually be enforced. A
/// memory-cache entry past `MEMORY_CACHE_TTL_SECS` should be treated as a
/// miss (re-fetched from Redis) rather than served indefinitely until LRU
/// capacity happens to evict it.
#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_memory_cache_entry_expires_after_configured_ttl() {
    // SAFETY: test-only env var set before any QueryCache in this process is
    // constructed; no concurrent access.
    unsafe {
        std::env::set_var("MEMORY_CACHE_TTL_SECS", "1");
    }
    let cache = QueryCache::new("redis://localhost:6379").await.unwrap();
    unsafe {
        std::env::remove_var("MEMORY_CACHE_TTL_SECS");
    }

    let key = "test:ttl:expiry";
    cache
        .set(
            key,
            &"value".to_string(),
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();

    // Immediately after set, the value is served from the in-memory LRU.
    let before_hits = cache.metrics().memory_hits;
    let hit: Option<String> = cache.get(key).await.unwrap();
    assert_eq!(hit, Some("value".to_string()));
    assert_eq!(
        cache.metrics().memory_hits,
        before_hits + 1,
        "expected an in-memory hit before the TTL elapses"
    );

    // After the 1s memory TTL elapses, the same key must fall through to
    // Redis (a memory miss) instead of being served from the stale entry.
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let memory_misses_before = cache.metrics().memory_misses;
    let still_there: Option<String> = cache.get(key).await.unwrap();
    assert_eq!(
        still_there,
        Some("value".to_string()),
        "value should still be served (from Redis) after the memory TTL expires"
    );
    assert_eq!(
        cache.metrics().memory_misses,
        memory_misses_before + 1,
        "expired in-memory entry should count as a memory miss, not a hit"
    );

    cache.invalidate_exact(key).await.ok();
}

#[test]
fn test_cache_config_defaults() {
    let config = CacheConfig::default();
    assert_eq!(config.status_counts_ttl, 300);
    assert_eq!(config.daily_totals_ttl, 3600);
    assert_eq!(config.asset_stats_ttl, 600);
}
