use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::services::feature_flags::FeatureFlagService;
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

#[tokio::test]
async fn test_flag_evaluation_enabled() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    sqlx::query("UPDATE feature_flags SET enabled = true WHERE name = 'experimental_processor'")
        .execute(&pool)
        .await
        .unwrap();

    let is_enabled = service.is_enabled("experimental_processor").await.unwrap();
    assert!(is_enabled);
}

#[tokio::test]
async fn test_flag_evaluation_disabled() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    sqlx::query("UPDATE feature_flags SET enabled = false WHERE name = 'new_asset_support'")
        .execute(&pool)
        .await
        .unwrap();

    let is_enabled = service.is_enabled("new_asset_support").await.unwrap();
    assert!(!is_enabled);
}

#[tokio::test]
async fn test_flag_cache_refresh() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let initial = service.is_enabled("experimental_processor").await.unwrap();

    sqlx::query(
        "UPDATE feature_flags SET enabled = NOT enabled WHERE name = 'experimental_processor'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let after_update = service.is_enabled("experimental_processor").await.unwrap();
    assert_ne!(initial, after_update);
}

#[tokio::test]
async fn test_flag_update_via_api() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let result = service
        .update("experimental_processor", true)
        .await
        .unwrap();
    assert_eq!(result.name, "experimental_processor");
    assert!(result.enabled);

    let is_enabled = service.is_enabled("experimental_processor").await.unwrap();
    assert!(is_enabled);

    let result = service
        .update("experimental_processor", false)
        .await
        .unwrap();
    assert!(!result.enabled);

    let is_enabled = service.is_enabled("experimental_processor").await.unwrap();
    assert!(!is_enabled);
}

#[tokio::test]
async fn test_flag_evaluation_performance() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = service.is_enabled("experimental_processor").await;
    }
    let duration = start.elapsed();

    assert!(
        duration.as_millis() < 5000,
        "1000 flag checks took {:?}",
        duration
    );
}

#[tokio::test]
async fn test_flag_default_values() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let nonexistent = service.is_enabled("nonexistent_flag").await.unwrap();
    assert!(!nonexistent);

    let flags = service.get_all_flags().await.unwrap();
    assert!(flags.contains_key("experimental_processor"));
    assert!(flags.contains_key("new_asset_support"));
}
