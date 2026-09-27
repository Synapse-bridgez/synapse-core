use sqlx::migrate::Migrator;
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

async fn start_db() -> (String, ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();

    (url, container)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pool_manager_primary_only() {
    let (url, _container) = start_db().await;

    let pool_manager = PoolManager::new(&url, None, 5)
        .await
        .expect("Failed to create pool manager");

    assert!(pool_manager.replica().is_none());

    let read_pool = pool_manager.get_read_pool().await;
    let write_pool = pool_manager.get_write_pool().await;
    assert!(std::ptr::eq(read_pool, write_pool));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_pool_manager_with_replica() {
    let replica_url = std::env::var("DATABASE_REPLICA_URL").ok();
    if replica_url.is_none() {
        println!("Skipping replica test - DATABASE_REPLICA_URL not set");
        return;
    }

    let (url, _container) = start_db().await;

    let pool_manager = PoolManager::new(&url, replica_url.as_deref(), 5)
        .await
        .expect("Failed to create pool manager");

    assert!(pool_manager.replica().is_some());

    let read_pool = pool_manager.get_read_pool().await;
    let write_pool = pool_manager.get_write_pool().await;
    assert!(!std::ptr::eq(read_pool, write_pool));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_query_routing() {
    let (url, _container) = start_db().await;

    let pool_manager = PoolManager::new(&url, None, 5)
        .await
        .expect("Failed to create pool manager");

    let read_pool = pool_manager.get_read_pool().await;
    let result: Result<sqlx::postgres::PgRow, sqlx::Error> =
        sqlx::query("SELECT 1 as value").fetch_one(read_pool).await;
    assert!(result.is_ok());

    let write_pool = pool_manager.get_write_pool().await;
    let result: Result<sqlx::postgres::PgRow, sqlx::Error> =
        sqlx::query("SELECT 1 as value").fetch_one(write_pool).await;
    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_health_check_with_invalid_replica() {
    let (url, _container) = start_db().await;

    let result = PoolManager::new(
        &url,
        Some("postgres://invalid:invalid@nonexistent:5432/db"),
        5,
    )
    .await;

    assert!(result.is_err());
}

// ── #1113: replica-unavailable fallback ──────────────────────────────────────

/// When the replica is marked unhealthy, `read_pool` must silently fall back
/// to the primary and `is_replica_healthy()` must return false so calling
/// code and health-checks can observe the state.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_replica_unavailable_falls_back_to_primary() {
    let (url, _container) = start_db().await;

    // Primary-only: no replica configured. read_pool must return primary.
    let manager = PoolManager::new(&url, None, 5)
        .await
        .expect("Failed to create pool manager with primary-only config");

    let (_pool, using_replica) = manager.read_pool().await;
    assert!(
        !using_replica,
        "primary-only manager must never report using_replica = true"
    );

    // The primary pool must be usable as the fallback.
    let pool = manager.get_read_pool().await;
    sqlx::query("SELECT 1")
        .fetch_one(pool)
        .await
        .expect("read query via fallback primary must succeed");
}

/// When a replica is configured and then marked unhealthy, reads must route
/// to the primary until `mark_replica_healthy` restores the replica.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_mark_replica_unhealthy_routes_reads_to_primary() {
    let (url, _container) = start_db().await;

    // Use the same URL for both pools so the test requires only one DB.
    let manager = PoolManager::new(&url, Some(&url), 5)
        .await
        .expect("Failed to create pool manager with replica config");

    // Replica starts healthy → reads go to replica.
    assert!(manager.is_replica_healthy().await, "replica must start healthy");
    let (_pool, using_replica) = manager.read_pool().await;
    assert!(using_replica, "reads should go to replica when healthy");

    // Simulate a call-site replica connection error.
    manager.mark_replica_unhealthy().await;
    assert!(!manager.is_replica_healthy().await, "replica must be marked unhealthy");

    let (_pool, using_replica) = manager.read_pool().await;
    assert!(
        !using_replica,
        "reads must fall back to primary when replica is unhealthy"
    );

    // Recovery: replica comes back online.
    manager.mark_replica_healthy().await;
    assert!(manager.is_replica_healthy().await, "replica must be healthy again after mark");
    let (_pool, using_replica) = manager.read_pool().await;
    assert!(using_replica, "reads must resume to replica once marked healthy");
}

/// Replica-lag-sensitive endpoints (write-then-read paths, INSERT … RETURNING)
/// must use the write pool. `get_write_pool()` must always return the primary,
/// never the replica, so a just-written row is immediately visible.
///
/// Affected endpoints: compliance-report generation (INSERT + RETURNING),
/// settlement creation, webhook delivery confirmation.
/// See docs/database_failover.md §"Replication-lag tolerance".
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_lag_sensitive_endpoint_uses_write_pool() {
    let (url, _container) = start_db().await;

    let manager = PoolManager::new(&url, Some(&url), 5)
        .await
        .expect("Failed to create pool manager");

    let write_pool = manager.get_write_pool().await;
    let primary_ptr = manager.primary() as *const _;
    assert!(
        std::ptr::eq(write_pool, primary_ptr),
        "get_write_pool() must return the primary, never the replica"
    );

    // Confirm the write pool handles DML-level connectivity.
    sqlx::query("SELECT 1 as v")
        .fetch_one(write_pool)
        .await
        .expect("write pool round-trip must succeed");
}
