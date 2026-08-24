//! Part E.1 regression test: driving a transaction through completion via
//! the live processor.rs::process_batch path must actually enqueue an
//! outbound webhook delivery. Before this PR, nothing in the live app ever
//! called WebhookDispatcher::enqueue() — this is the test that would have
//! caught that immediately.

use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::db::queries::insert_transaction;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::services::processor::process_batch;
use synapse_core::services::webhook_dispatcher::WebhookDispatcher;
use synapse_core::stellar::HorizonClient;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

#[path = "fixtures.rs"]
mod fixtures;
use fixtures::TransactionFixture;

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

fn test_redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

#[ignore = "Requires Docker and Redis"]
#[tokio::test]
async fn test_completed_transaction_enqueues_webhook_delivery() {
    let (pool, _container) = setup_test_db().await;

    let account = "GWEBHOOKENQUEUETEST";

    // A pending transaction the live processor will claim and complete.
    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_status("pending")
        .with_asset_code("USD")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    // An endpoint subscribed to the completion event.
    let endpoint_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, url, secret, event_types, enabled, max_delivery_rate) \
         VALUES ($1, $2, $3, $4, true, 100)",
    )
    .bind(endpoint_id)
    .bind("https://example.invalid/webhook")
    .bind("test-secret")
    .bind(vec!["transaction.completed".to_string()])
    .execute(&pool)
    .await
    .unwrap();

    // Enable the enqueue-on-completion flag at 100% — this is normally off
    // by default (see migration 20260823000002); this test exercises the
    // fully-rolled-out state.
    sqlx::query(
        "INSERT INTO feature_flags (name, enabled, rollout_percentage) \
         VALUES ('webhook_enqueue_on_completion', true, 100) \
         ON CONFLICT (name) DO UPDATE SET enabled = true, rollout_percentage = 100",
    )
    .execute(&pool)
    .await
    .unwrap();

    let redis_url = test_redis_url();
    let dispatcher = WebhookDispatcher::new(pool.clone(), &redis_url).unwrap();
    let feature_flags = FeatureFlagService::new(pool.clone());
    let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());

    let processed = process_batch(
        &pool,
        &horizon_client,
        10,
        Some(&dispatcher),
        None,
        &feature_flags,
    )
    .await
    .unwrap();
    assert!(
        processed >= 1,
        "expected at least one transaction to be completed"
    );

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed");

    let delivery = sqlx::query(
        "SELECT id FROM webhook_deliveries WHERE transaction_id = $1 AND endpoint_id = $2",
    )
    .bind(tx_id)
    .bind(endpoint_id)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(
        delivery.is_some(),
        "expected process_batch to have enqueued a webhook_deliveries row for the completed transaction"
    );
    let _ = delivery.map(|r| r.get::<uuid::Uuid, _>("id"));
}

/// Confirms the default-off flag actually gates delivery: with the flag
/// left at its migration default (enabled = false), completing a
/// transaction must NOT enqueue a delivery, so merging this PR doesn't
/// silently activate webhook delivery fleet-wide.
#[ignore = "Requires Docker and Redis"]
#[tokio::test]
async fn test_enqueue_stays_off_until_flag_enabled() {
    let (pool, _container) = setup_test_db().await;
    let account = "GWEBHOOKFLAGOFFTEST";

    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_status("pending")
        .with_asset_code("USD")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    let endpoint_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, url, secret, event_types, enabled, max_delivery_rate) \
         VALUES ($1, $2, $3, $4, true, 100)",
    )
    .bind(endpoint_id)
    .bind("https://example.invalid/webhook")
    .bind("test-secret")
    .bind(vec!["transaction.completed".to_string()])
    .execute(&pool)
    .await
    .unwrap();

    // Deliberately do NOT enable the flag — rely on the migration's default.
    let redis_url = test_redis_url();
    let dispatcher = WebhookDispatcher::new(pool.clone(), &redis_url).unwrap();
    let feature_flags = FeatureFlagService::new(pool.clone());
    let horizon_client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());

    process_batch(
        &pool,
        &horizon_client,
        10,
        Some(&dispatcher),
        None,
        &feature_flags,
    )
    .await
    .unwrap();

    let status: String = sqlx::query_scalar("SELECT status FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "completed", "the transaction should still complete");

    let delivery = sqlx::query("SELECT id FROM webhook_deliveries WHERE transaction_id = $1")
        .bind(tx_id)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(
        delivery.is_none(),
        "expected no webhook delivery to be enqueued while the rollout flag is off"
    );
}
