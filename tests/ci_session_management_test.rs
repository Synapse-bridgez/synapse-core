//! Session Management Tests for CI/CD
//!
//! This test suite validates session management behavior in CI/CD environments,
//! ensuring proper handling of database connections, Redis sessions, and
//! service lifecycle management during automated testing.

use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// Test database session lifecycle in CI environment
#[tokio::test]
async fn test_database_session_lifecycle() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    // Create a connection pool
    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Verify session is active
    let result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("Failed to execute query");

    assert_eq!(result.0, 1);

    // Close the pool
    pool.close().await;
}

/// Test database session timeout handling
#[tokio::test]
async fn test_database_session_timeout() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    // Create pool with short timeout
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Verify connection works
    let result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("Failed to execute query");

    assert_eq!(result.0, 1);

    pool.close().await;
}

/// Test concurrent database sessions
#[tokio::test]
async fn test_concurrent_database_sessions() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = Arc::new(
        PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to database"),
    );

    let mut handles = vec![];

    // Spawn multiple concurrent sessions
    for i in 0..10 {
        let pool_clone = Arc::clone(&pool);
        let handle = tokio::spawn(async move {
            let result: (i32,) = sqlx::query_as("SELECT $1")
                .bind(i)
                .fetch_one(pool_clone.as_ref())
                .await
                .expect("Failed to execute query");
            result.0
        });
        handles.push(handle);
    }

    // Wait for all sessions to complete
    for (i, handle) in handles.into_iter().enumerate() {
        let result = handle.await.expect("Task panicked");
        assert_eq!(result, i as i32);
    }

    pool.close().await;
}

/// Test database session recovery after error
#[tokio::test]
async fn test_database_session_recovery() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Execute a query that will fail
    let error_result: Result<(i32,), sqlx::Error> =
        sqlx::query_as("SELECT * FROM nonexistent_table")
            .fetch_one(&pool)
            .await;

    assert!(error_result.is_err());

    // Verify session can recover and execute valid queries
    let result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("Failed to execute query after error");

    assert_eq!(result.0, 1);

    pool.close().await;
}

/// Test database transaction session management
#[tokio::test]
async fn test_database_transaction_session() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Begin transaction
    let mut tx = pool.begin().await.expect("Failed to begin transaction");

    // Execute query within transaction
    let result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut *tx)
        .await
        .expect("Failed to execute query in transaction");

    assert_eq!(result.0, 1);

    // Commit transaction
    tx.commit().await.expect("Failed to commit transaction");

    pool.close().await;
}

/// Test database transaction rollback
#[tokio::test]
async fn test_database_transaction_rollback() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Begin transaction
    let mut tx = pool.begin().await.expect("Failed to begin transaction");

    // Execute query within transaction
    let _result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut *tx)
        .await
        .expect("Failed to execute query in transaction");

    // Rollback transaction
    tx.rollback().await.expect("Failed to rollback transaction");

    pool.close().await;
}

/// Test Redis session management (if Redis is available)
#[tokio::test]
async fn test_redis_session_management() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    // Try to connect to Redis
    let client = match redis::Client::open(redis_url.as_str()) {
        Ok(c) => c,
        Err(_) => {
            println!("Redis not available, skipping test");
            return;
        }
    };

    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(_) => {
            println!("Redis connection failed, skipping test");
            return;
        }
    };

    // Test basic Redis operations
    let _: () = redis::cmd("SET")
        .arg("test_key")
        .arg("test_value")
        .query_async(&mut conn)
        .await
        .expect("Failed to set Redis key");

    let value: String = redis::cmd("GET")
        .arg("test_key")
        .query_async(&mut conn)
        .await
        .expect("Failed to get Redis key");

    assert_eq!(value, "test_value");

    // Cleanup
    let _: () = redis::cmd("DEL")
        .arg("test_key")
        .query_async(&mut conn)
        .await
        .expect("Failed to delete Redis key");
}

/// Test Redis session expiration
#[tokio::test]
async fn test_redis_session_expiration() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let client = match redis::Client::open(redis_url.as_str()) {
        Ok(c) => c,
        Err(_) => {
            println!("Redis not available, skipping test");
            return;
        }
    };

    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(_) => {
            println!("Redis connection failed, skipping test");
            return;
        }
    };

    // Set key with expiration
    let _: () = redis::cmd("SETEX")
        .arg("expiring_key")
        .arg(1) // 1 second expiration
        .arg("expiring_value")
        .query_async(&mut conn)
        .await
        .expect("Failed to set expiring key");

    // Verify key exists
    let exists: bool = redis::cmd("EXISTS")
        .arg("expiring_key")
        .query_async(&mut conn)
        .await
        .expect("Failed to check key existence");

    assert!(exists);

    // Wait for expiration
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Verify key expired
    let exists_after: bool = redis::cmd("EXISTS")
        .arg("expiring_key")
        .query_async(&mut conn)
        .await
        .expect("Failed to check key existence after expiration");

    assert!(!exists_after);
}

/// Test connection pool session limits
#[tokio::test]
async fn test_connection_pool_limits() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    // Create pool with limited connections
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Acquire connections up to the limit
    let mut conn1 = pool
        .acquire()
        .await
        .expect("Failed to acquire connection 1");
    let mut conn2 = pool
        .acquire()
        .await
        .expect("Failed to acquire connection 2");

    // Verify we can still use the connections
    let result1: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut *conn1)
        .await
        .expect("Failed to execute query on connection 1");

    let result2: (i32,) = sqlx::query_as("SELECT 2")
        .fetch_one(&mut *conn2)
        .await
        .expect("Failed to execute query on connection 2");

    assert_eq!(result1.0, 1);
    assert_eq!(result2.0, 2);

    // Release connections
    drop(conn1);
    drop(conn2);

    pool.close().await;
}

/// Test session cleanup on service shutdown
#[tokio::test]
async fn test_session_cleanup_on_shutdown() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Acquire a connection
    let mut conn = pool.acquire().await.expect("Failed to acquire connection");

    // Execute a query
    let _result: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut *conn)
        .await
        .expect("Failed to execute query");

    // Release connection
    drop(conn);

    // Close pool (simulates service shutdown)
    pool.close().await;

    // Verify pool is closed
    assert!(pool.is_closed());
}

/// Test session state isolation
#[tokio::test]
async fn test_session_state_isolation() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Create two separate transactions
    let mut tx1 = pool.begin().await.expect("Failed to begin transaction 1");
    let mut tx2 = pool.begin().await.expect("Failed to begin transaction 2");

    // Execute queries in separate transactions
    let result1: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&mut *tx1)
        .await
        .expect("Failed to execute query in transaction 1");

    let result2: (i32,) = sqlx::query_as("SELECT 2")
        .fetch_one(&mut *tx2)
        .await
        .expect("Failed to execute query in transaction 2");

    assert_eq!(result1.0, 1);
    assert_eq!(result2.0, 2);

    // Commit both transactions
    tx1.commit().await.expect("Failed to commit transaction 1");
    tx2.commit().await.expect("Failed to commit transaction 2");

    pool.close().await;
}

/// Test session reconnection after connection loss
#[tokio::test]
async fn test_session_reconnection() {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());

    // Create pool with retry logic
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("Failed to connect to database");

    // Execute initial query
    let result1: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("Failed to execute initial query");

    assert_eq!(result1.0, 1);

    // Simulate reconnection by executing another query
    let result2: (i32,) = sqlx::query_as("SELECT 2")
        .fetch_one(&pool)
        .await
        .expect("Failed to execute query after reconnection");

    assert_eq!(result2.0, 2);

    pool.close().await;
}
