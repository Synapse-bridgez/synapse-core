use synapse_core::db::pool_manager::{PoolManager, QueryIntent};

#[tokio::test]
async fn test_pool_manager_primary_only() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool_manager = PoolManager::new(&database_url, None)
        .await
        .expect("Failed to create pool manager");

    // Verify replica is None
    assert!(pool_manager.replica().is_none());

    // Verify read queries use primary
    let read_pool = pool_manager.get_pool(QueryIntent::Read);
    let write_pool = pool_manager.get_pool(QueryIntent::Write);
    
    // Both should point to same pool (primary)
    assert!(std::ptr::eq(read_pool, write_pool));

    // Health check should succeed
    let health = pool_manager.health_check().await;
    assert!(health.primary);
    assert!(health.replica); // Should be true when no replica configured
}

#[tokio::test]
async fn test_pool_manager_with_replica() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());
    
    let replica_url = std::env::var("DATABASE_REPLICA_URL").ok();

    if replica_url.is_none() {
        println!("Skipping replica test - DATABASE_REPLICA_URL not set");
        return;
    }

    let pool_manager = PoolManager::new(&database_url, replica_url.as_deref())
        .await
        .expect("Failed to create pool manager");

    // Verify replica is configured
    assert!(pool_manager.replica().is_some());

    // Verify read and write use different pools
    let read_pool = pool_manager.get_pool(QueryIntent::Read);
    let write_pool = pool_manager.get_pool(QueryIntent::Write);
    
    assert!(!std::ptr::eq(read_pool, write_pool));

    // Health check
    let health = pool_manager.health_check().await;
    assert!(health.primary);
    assert!(health.replica);
}

#[tokio::test]
async fn test_query_routing() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool_manager = PoolManager::new(&database_url, None)
        .await
        .expect("Failed to create pool manager");

    // Test read query
    let read_pool = pool_manager.get_pool(QueryIntent::Read);
    let result = sqlx::query("SELECT 1 as value")
        .fetch_one(read_pool)
        .await;
    assert!(result.is_ok());

    // Test write query
    let write_pool = pool_manager.get_pool(QueryIntent::Write);
    let result = sqlx::query("SELECT 1 as value")
        .fetch_one(write_pool)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_health_check_with_invalid_replica() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    // Try to create with invalid replica URL
    let result = PoolManager::new(
        &database_url,
        Some("postgres://invalid:invalid@nonexistent:5432/db"),
    )
    .await;

    // Should fail to connect to invalid replica
    assert!(result.is_err());
}
