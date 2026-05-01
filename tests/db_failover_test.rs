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

    let pool_manager = PoolManager::new(&url, None)
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

    let pool_manager = PoolManager::new(&url, replica_url.as_deref())
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

    let pool_manager = PoolManager::new(&url, None)
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
    )
    .await;

    assert!(result.is_err());
}
