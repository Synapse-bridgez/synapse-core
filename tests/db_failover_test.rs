use synapse_core::db::pool_manager::PoolManager;

#[tokio::test]
async fn test_pool_manager_primary_only() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DB failover test: DATABASE_URL not set");
            return;
        }
    };

    let pool_manager = PoolManager::new(&database_url, None)
        .await
        .expect("Failed to create pool manager");

    // Verify replica is None
    assert!(pool_manager.replica().is_none());

    // Verify read queries use primary
    let read_pool = pool_manager.get_read_pool().await;
    let write_pool = pool_manager.get_write_pool().await;
    
    // Both should point to same pool (primary)
    assert!(std::ptr::eq(read_pool, write_pool));

}

#[tokio::test]
async fn test_pool_manager_with_replica() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DB failover test: DATABASE_URL not set");
            return;
        }
    };
    
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
    let read_pool = pool_manager.get_read_pool().await;
    let write_pool = pool_manager.get_write_pool().await;
    
    assert!(!std::ptr::eq(read_pool, write_pool));

}

#[tokio::test]
async fn test_query_routing() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DB failover test: DATABASE_URL not set");
            return;
        }
    };

    let pool_manager = PoolManager::new(&database_url, None)
        .await
        .expect("Failed to create pool manager");

    // Test read query
    let read_pool = pool_manager.get_read_pool().await;
    let result: Result<sqlx::postgres::PgRow, sqlx::Error> = sqlx::query("SELECT 1 as value")
        .fetch_one(read_pool)
        .await;
    assert!(result.is_ok());

    // Test write query
    let write_pool = pool_manager.get_write_pool().await;
    let result: Result<sqlx::postgres::PgRow, sqlx::Error> = sqlx::query("SELECT 1 as value")
        .fetch_one(write_pool)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_health_check_with_invalid_replica() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DB failover test: DATABASE_URL not set");
            return;
        }
    };

    // Try to create with invalid replica URL
    let result = PoolManager::new(
        &database_url,
        Some("postgres://invalid:invalid@nonexistent:5432/db"),
    )
    .await;

    // Should fail to connect to invalid replica
    assert!(result.is_err());
}
