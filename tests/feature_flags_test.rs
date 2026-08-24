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

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_flag_evaluation_enabled() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    sqlx::query("UPDATE feature_flags SET enabled = true WHERE name = 'experimental_processor'")
        .execute(&pool)
        .await
        .unwrap();

    let is_enabled = service
        .is_enabled_ignoring_rollout("experimental_processor")
        .await
        .unwrap();
    assert!(is_enabled);
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_flag_evaluation_disabled() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    sqlx::query("UPDATE feature_flags SET enabled = false WHERE name = 'new_asset_support'")
        .execute(&pool)
        .await
        .unwrap();

    let is_enabled = service
        .is_enabled_ignoring_rollout("new_asset_support")
        .await
        .unwrap();
    assert!(!is_enabled);
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_flag_cache_refresh() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let initial = service
        .is_enabled_ignoring_rollout("experimental_processor")
        .await
        .unwrap();

    sqlx::query(
        "UPDATE feature_flags SET enabled = NOT enabled WHERE name = 'experimental_processor'",
    )
    .execute(&pool)
    .await
    .unwrap();

    let after_update = service
        .is_enabled_ignoring_rollout("experimental_processor")
        .await
        .unwrap();
    assert_ne!(initial, after_update);
}

#[ignore = "Requires Docker/external services"]
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

    let is_enabled = service
        .is_enabled_ignoring_rollout("experimental_processor")
        .await
        .unwrap();
    assert!(is_enabled);

    let result = service
        .update("experimental_processor", false)
        .await
        .unwrap();
    assert!(!result.enabled);

    let is_enabled = service
        .is_enabled_ignoring_rollout("experimental_processor")
        .await
        .unwrap();
    assert!(!is_enabled);
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_flag_evaluation_performance() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let start = std::time::Instant::now();
    for _ in 0..1000 {
        let _ = service
            .is_enabled_ignoring_rollout("experimental_processor")
            .await;
    }
    let duration = start.elapsed();

    assert!(
        duration.as_millis() < 5000,
        "1000 flag checks took {:?}",
        duration
    );
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_flag_default_values() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    let nonexistent = service
        .is_enabled_ignoring_rollout("nonexistent_flag")
        .await
        .unwrap();
    assert!(!nonexistent);

    let flags = service.get_all_flags().await.unwrap();
    assert!(flags.contains_key("experimental_processor"));
    assert!(flags.contains_key("new_asset_support"));
}

// ── Part D regression: rollout_percentage must actually be respected ──────────

/// Guards against the bug where `TransactionProcessor`'s EnrichStage/VerifyStage
/// gating called the non-percentage-aware `is_enabled_ignoring_rollout`, so an
/// operator-configured `rollout_percentage: 10` silently applied to 100% of
/// traffic instead of ~10%. `is_enabled_for_key` is what those call sites use
/// now (keyed on stellar_account); this drives it across many distinct keys
/// and checks the activation rate lands near the configured percentage.
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_rollout_percentage_respects_configured_percentage_not_all_or_nothing() {
    let (pool, _container) = setup_test_db().await;
    let service = FeatureFlagService::new(pool.clone());

    sqlx::query(
        "INSERT INTO feature_flags (name, enabled, rollout_percentage) \
         VALUES ('transaction_enrich_stage', true, 10) \
         ON CONFLICT (name) DO UPDATE SET enabled = true, rollout_percentage = 10",
    )
    .execute(&pool)
    .await
    .unwrap();

    const SAMPLE_SIZE: usize = 500;
    let mut enabled_count = 0;
    for i in 0..SAMPLE_SIZE {
        let account = format!("GACCOUNT{i}");
        if service
            .is_enabled_for_key("transaction_enrich_stage", &account)
            .await
            .unwrap()
        {
            enabled_count += 1;
        }
    }

    let percentage = (enabled_count as f64 / SAMPLE_SIZE as f64) * 100.0;
    assert!(
        (5.0..=15.0).contains(&percentage),
        "expected roughly 10% of accounts to have the stage enabled (not ~100%), \
         got {percentage}% ({enabled_count}/{SAMPLE_SIZE})"
    );

    // Confirm the same key is always stable (deterministic rollout, not a
    // coin flip per call) — required for a gradual rollout to be meaningful.
    let repeat = service
        .is_enabled_for_key("transaction_enrich_stage", "GACCOUNT0")
        .await
        .unwrap();
    let repeat_again = service
        .is_enabled_for_key("transaction_enrich_stage", "GACCOUNT0")
        .await
        .unwrap();
    assert_eq!(
        repeat, repeat_again,
        "the same key must get a stable answer"
    );
}
