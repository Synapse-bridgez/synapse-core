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

// ---------------------------------------------------------------------------
// #1097 — v2 two-source cross-check tests
// ---------------------------------------------------------------------------

/// Enables both v1 and v2 payment verification flags at 100% rollout.
async fn enable_payment_verification_v2(pool: &PgPool) {
    enable_payment_verification(pool).await;
    sqlx::query(
        "UPDATE feature_flags SET enabled = true, rollout_percentage = 100 \
         WHERE name = 'payment_verification_v2'",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Helper: set anchor callback fields on a transaction to simulate a received anchor signal.
async fn set_anchor_confirmed(pool: &PgPool, tx_id: uuid::Uuid, anchor_tx_id: &str) {
    sqlx::query(
        "UPDATE transactions \
         SET anchor_transaction_id = $2, callback_status = 'completed', callback_type = 'deposit', \
             updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(tx_id)
    .bind(anchor_tx_id)
    .execute(pool)
    .await
    .unwrap();
}

/// When both Horizon and anchor agree (both signals present), the transaction
/// must transition to `completed` with `verification_source = 'horizon+anchor'`.
#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_v2_both_signals_agree_completes_transaction() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification_v2(&pool).await;

    let account = "GV2AGREE11111111111111111111111111111111111111111111111";
    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_amount("200.00")
        .with_asset_code("USD")
        .with_status("pending")
        .with_memo("agree-memo-1", "text")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    // Signal 2: anchor confirmed
    set_anchor_confirmed(&pool, tx_id, "anchor-tx-agree-001").await;

    // Signal 1: Horizon returns a matching payment
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&format!(
            r#"{{"_embedded": {{"records": [
                {{"id": "hz-agree-001", "from": "GSENDER", "to": "{account}",
                  "amount": "200.0000000", "asset_code": "USD", "memo": "agree-memo-1"}}
            ]}}}}"#
        ))
        .create_async()
        .await;

    let horizon_client = HorizonClient::new(server.url());
    let feature_flags = FeatureFlagService::new(pool.clone());

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .expect("process_batch should not error");

    let fetched = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(
        fetched.status, "completed",
        "transaction must be completed when both signals agree"
    );
}

/// When Horizon matches but anchor callback has not arrived yet, the
/// transaction must remain `pending` (deferred — waiting for signal 2).
#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_v2_horizon_matches_anchor_absent_defers() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification_v2(&pool).await;

    let account = "GV2DEFER11111111111111111111111111111111111111111111111";
    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_amount("50.00")
        .with_asset_code("XLM")
        .with_status("pending")
        .with_memo("defer-memo-1", "text")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();
    // No anchor callback set — signal 2 is absent.

    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&format!(
            r#"{{"_embedded": {{"records": [
                {{"id": "hz-defer-001", "from": "GSENDER", "to": "{account}",
                  "amount": "50.0000000", "asset_code": "XLM", "memo": "defer-memo-1"}}
            ]}}}}"#
        ))
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
        "transaction must remain pending when anchor callback has not arrived"
    );
}

/// When anchor is confirmed but Horizon shows no matching payment yet,
/// the transaction must remain `pending` (deferred — waiting for signal 1).
/// This tests the out-of-order arrival path: anchor arrives before Horizon.
#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_v2_anchor_confirmed_horizon_absent_defers_out_of_order() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification_v2(&pool).await;

    let account = "GV2OUTOFOR1111111111111111111111111111111111111111111";
    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_amount("75.00")
        .with_asset_code("USDC")
        .with_status("pending")
        .with_memo("ooo-memo-1", "text")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    // Signal 2 arrives first: anchor confirms.
    set_anchor_confirmed(&pool, tx_id, "anchor-tx-ooo-001").await;

    // Signal 1: Horizon returns no matching payment (not arrived yet).
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()))
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
        "transaction must remain pending when Horizon payment has not arrived yet (out-of-order)"
    );
}

/// When out-of-order arrival resolves on the second tick (Horizon arrives
/// after anchor), the transaction must complete.
#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_v2_out_of_order_resolves_on_second_tick() {
    let (pool, _container) = setup_test_db().await;
    enable_payment_verification_v2(&pool).await;

    let account = "GV2OOO2ND11111111111111111111111111111111111111111111";
    let tx = TransactionFixture::new()
        .with_stellar_account(account)
        .with_amount("120.00")
        .with_asset_code("USD")
        .with_status("pending")
        .with_memo("ooo2-memo", "text")
        .build();
    let tx_id = tx.id;
    insert_transaction(&pool, &tx, None).await.unwrap();

    // Anchor arrives first.
    set_anchor_confirmed(&pool, tx_id, "anchor-tx-ooo2-001").await;

    let mut server = mockito::Server::new_async().await;
    let horizon_client = HorizonClient::new(server.url());
    let feature_flags = FeatureFlagService::new(pool.clone());

    // Tick 1: Horizon has no payment yet.
    let empty_mock = server
        .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"_embedded": {"records": []}}"#)
        .create_async()
        .await;

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .unwrap();

    let after_tick_1 = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(after_tick_1.status, "pending");
    empty_mock.remove_async().await;

    // Tick 2: Horizon now has a matching payment.
    let _match_mock = server
        .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&format!(
            r#"{{"_embedded": {{"records": [
                {{"id": "hz-ooo2-001", "from": "GSENDER", "to": "{account}",
                  "amount": "120.0000000", "asset_code": "USD", "memo": "ooo2-memo"}}
            ]}}}}"#
        ))
        .create_async()
        .await;

    process_batch(&pool, &horizon_client, 10, None, None, &feature_flags)
        .await
        .unwrap();

    let after_tick_2 = get_transaction(&pool, tx_id).await.unwrap();
    assert_eq!(
        after_tick_2.status, "completed",
        "transaction must complete once both signals agree"
    );
}

// ---------------------------------------------------------------------------
// Unit tests for cross_check_signals — no DB or network required
// ---------------------------------------------------------------------------

#[cfg(test)]
mod cross_check_unit {
    use synapse_core::services::processor::{
        cross_check_signals, AnchorSignal, PaymentLookup, VerificationV2Decision,
    };

    #[test]
    fn test_both_agree_returns_complete() {
        let decision = cross_check_signals(
            &PaymentLookup::Matched("hz-001".into()),
            &AnchorSignal::Confirmed,
        );
        assert!(
            matches!(decision, VerificationV2Decision::Complete { .. }),
            "both signals present should return Complete"
        );
    }

    #[test]
    fn test_horizon_match_anchor_absent_defers() {
        let decision = cross_check_signals(
            &PaymentLookup::Matched("hz-001".into()),
            &AnchorSignal::Absent,
        );
        assert!(
            matches!(decision, VerificationV2Decision::Defer { .. }),
            "anchor absent should defer"
        );
    }

    #[test]
    fn test_anchor_confirmed_no_horizon_defers() {
        let decision = cross_check_signals(
            &PaymentLookup::NoMatchingPayment,
            &AnchorSignal::Confirmed,
        );
        assert!(
            matches!(decision, VerificationV2Decision::Defer { .. }),
            "horizon missing should defer (not disagree — could be transient)"
        );
    }

    #[test]
    fn test_both_absent_defers() {
        let decision = cross_check_signals(
            &PaymentLookup::NoMatchingPayment,
            &AnchorSignal::Absent,
        );
        assert!(
            matches!(decision, VerificationV2Decision::Defer { .. }),
            "both signals absent should defer"
        );
    }

    #[test]
    fn test_lookup_failed_anchor_confirmed_defers() {
        // LookupFailed is transient (network/CB) — not a disagreement.
        let decision = cross_check_signals(
            &PaymentLookup::LookupFailed,
            &AnchorSignal::Confirmed,
        );
        assert!(
            matches!(decision, VerificationV2Decision::Defer { .. }),
            "lookup failure with anchor confirmed should defer, not route to pending_review"
        );
    }
}
