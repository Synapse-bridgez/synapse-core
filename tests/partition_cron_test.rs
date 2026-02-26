use chrono::{Datelike, Utc};
use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::db::cron::{
    create_month_partition, detach_and_archive_old_partitions, ensure_future_partitions,
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

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

async fn partition_exists(pool: &PgPool, partition_name: &str) -> bool {
    let result = sqlx::query("SELECT 1 FROM pg_class WHERE relname = $1")
        .bind(partition_name)
        .fetch_optional(pool)
        .await
        .unwrap();
    result.is_some()
}

async fn get_partition_count(pool: &PgPool) -> i64 {
    let row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM pg_inherits i 
         JOIN pg_class c ON i.inhrelid = c.oid 
         JOIN pg_class p ON i.inhparent = p.oid 
         WHERE p.relname = 'transactions'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    row.get("cnt")
}

#[tokio::test]
async fn test_create_month_partition() {
    let (pool, _container) = setup_test_db().await;

    let year = 2025;
    let month = 3;

    let result = create_month_partition(&pool, year, month).await;
    assert!(result.is_ok());

    let partition_name = format!("transactions_y{}m{:02}", year, month);
    assert!(partition_exists(&pool, &partition_name).await);

    let idx1 = format!("idx_{}_status", partition_name);
    let idx2 = format!("idx_{}_stellar_account", partition_name);
    assert!(partition_exists(&pool, &idx1).await);
    assert!(partition_exists(&pool, &idx2).await);
}

#[tokio::test]
async fn test_create_month_partition_idempotent() {
    let (pool, _container) = setup_test_db().await;

    let year = 2025;
    let month = 6;

    create_month_partition(&pool, year, month).await.unwrap();
    let result = create_month_partition(&pool, year, month).await;
    assert!(result.is_ok());

    let partition_name = format!("transactions_y{}m{:02}", year, month);
    assert!(partition_exists(&pool, &partition_name).await);
}

#[tokio::test]
async fn test_ensure_future_partitions() {
    let (pool, _container) = setup_test_db().await;

    let initial_count = get_partition_count(&pool).await;

    let result = ensure_future_partitions(&pool, 3).await;
    assert!(result.is_ok());

    let final_count = get_partition_count(&pool).await;
    assert!(final_count >= initial_count + 3);

    let now = Utc::now();
    let partition_name = format!("transactions_y{}m{:02}", now.year(), now.month());
    assert!(partition_exists(&pool, &partition_name).await);
}

#[tokio::test]
async fn test_detach_old_partitions() {
    let (pool, _container) = setup_test_db().await;

    create_month_partition(&pool, 2023, 1).await.unwrap();
    create_month_partition(&pool, 2023, 2).await.unwrap();
    create_month_partition(&pool, 2025, 12).await.unwrap();

    let result = detach_and_archive_old_partitions(&pool, 12).await;
    assert!(result.is_ok());

    let schema_exists = sqlx::query("SELECT 1 FROM pg_namespace WHERE nspname = 'archive'")
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(schema_exists.is_some());

    let archived = sqlx::query(
        "SELECT COUNT(*) as cnt FROM pg_class c 
         JOIN pg_namespace n ON c.relnamespace = n.oid 
         WHERE n.nspname = 'archive' AND c.relname LIKE 'transactions_y%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let archived_count: i64 = archived.get("cnt");
    assert!(archived_count >= 2);
}

#[tokio::test]
async fn test_parse_partition_name() {
    let (pool, _container) = setup_test_db().await;

    create_month_partition(&pool, 2025, 5).await.unwrap();

    let rows = sqlx::query(
        "SELECT c.relname as child FROM pg_inherits i 
         JOIN pg_class c ON i.inhrelid = c.oid 
         JOIN pg_class p ON i.inhparent = p.oid 
         WHERE p.relname = 'transactions' AND c.relname LIKE 'transactions_y2025m05'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert!(!rows.is_empty());
    let child: String = rows[0].get("child");
    assert_eq!(child, "transactions_y2025m05");
}

#[tokio::test]
async fn test_partition_error_handling_invalid_month() {
    let (pool, _container) = setup_test_db().await;

    let result = create_month_partition(&pool, 2025, 13).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_partition_december_rollover() {
    let (pool, _container) = setup_test_db().await;

    let result = create_month_partition(&pool, 2025, 12).await;
    assert!(result.is_ok());

    let partition_name = "transactions_y2025m12";
    assert!(partition_exists(&pool, partition_name).await);
}

#[tokio::test]
async fn test_ensure_future_partitions_multiple_years() {
    let (pool, _container) = setup_test_db().await;

    let result = ensure_future_partitions(&pool, 15).await;
    assert!(result.is_ok());

    let count = get_partition_count(&pool).await;
    assert!(count >= 15);
}

#[tokio::test]
async fn test_partition_retention_boundary() {
    let (pool, _container) = setup_test_db().await;

    let now = Utc::now();
    let current_year = now.year();
    let current_month = now.month();

    create_month_partition(&pool, current_year, current_month)
        .await
        .unwrap();

    let result = detach_and_archive_old_partitions(&pool, 1).await;
    assert!(result.is_ok());

    let partition_name = format!("transactions_y{}m{:02}", current_year, current_month);
    assert!(partition_exists(&pool, &partition_name).await);
}
