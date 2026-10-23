//! Part A regression tests: `process_batch` must only complete a
//! transaction when Horizon reports a payment that actually matches it,
//! not merely because the destination account exists (or, prior to this
//! fix, with no Horizon check at all).

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries::{get_transaction, insert_transaction};
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::services::processor::process_batch;
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

/// Enables `payment_verification_enabled` at 100% rollout so the test
/// exercises the new verification-gated path rather than shadow mode.
async fn enable_payment_verification(pool: &PgPool) {
    sqlx::query(
        "UPDATE feature_flags SET enabled = true, rollout_percentage = 100 \
         WHERE name = 'payment_verification_enabled'",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_no_matching_payment_does_not_complete() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification(&pool).await;

    let tx = TransactionFixture::new()
        .with_stellar_account("GNOMATCH1111111111111111111111111111111111111111111111")
        .with_amount("100.00")
        .with_asset_code("USD")
        .with_status("pending")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    // Horizon reports the account exists (200) but with no payments that
    // match this transaction's amount/asset.
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"_embedded": {"records": []}}"#)
        .create_async()
        .await;

    let horizon_client = HorizonClient::new(server.url());
    let feature_flags = FeatureFlagService::new(pool.clone());

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .expect("process_batch should not error");

    let fetched = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(
        fetched.status, "pending",
        "transaction must not complete without a matching Horizon payment"
    );
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_account_not_found_then_funded_within_window_eventually_completes() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification(&pool).await;

    let tx = TransactionFixture::new()
        .with_stellar_account("GLATEFUND2222222222222222222222222222222222222222222222")
        .with_amount("50.00")
        .with_asset_code("XLM")
        .with_status("pending")
        .with_memo("invoice-42", "text")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    let mut server = mockito::Server::new_async().await;
    let horizon_client = HorizonClient::new(server.url());
    let feature_flags = FeatureFlagService::new(pool.clone());

    // Tick 1: destination account does not exist on-chain yet.
    let not_found_mock = server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
        )
        .with_status(404)
        .create_async()
        .await;

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .expect("process_batch should not error on account-not-found");

    let after_tick_1 = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(
        after_tick_1.status, "pending",
        "an unfunded destination account must not immediately fail the transaction"
    );

    not_found_mock.remove_async().await;

    // Tick 2: the account has since been funded with the expected payment.
    let matched_mock = server
        .mock(
            "GET",
            mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
        )
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"_embedded": {"records": [
                {"id": "999", "from": "GSENDER", "to": "GLATEFUND2222222222222222222222222222222222222222222222", "amount": "50.0000000", "asset_code": "XLM", "memo": "invoice-42"}
            ]}}"#,
        )
        .create_async()
        .await;

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .expect("process_batch should not error on the funded tick");

    let after_tick_2 = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(
        after_tick_2.status, "completed",
        "transaction must complete once a matching payment is found within the retry window"
    );

    matched_mock.assert_async().await;
}
