use synapse_core::services::{CacheConfig, QueryCache};

#[tokio::test]
async fn test_query_cache_basic_operations() {
    let cache = QueryCache::new("redis://localhost:6379").unwrap();

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

#[tokio::test]
async fn test_cache_metrics() {
    let cache = QueryCache::new("redis://localhost:6379").unwrap();

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

#[tokio::test]
async fn test_cache_invalidation() {
    let cache = QueryCache::new("redis://localhost:6379").unwrap();

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

#[test]
fn test_cache_config_defaults() {
    let config = CacheConfig::default();
    assert_eq!(config.status_counts_ttl, 300);
    assert_eq!(config.daily_totals_ttl, 3600);
    assert_eq!(config.asset_stats_ttl, 600);
}
