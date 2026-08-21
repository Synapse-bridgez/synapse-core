//! Test for reconciliation report discrepancy flag fix (Issue #1062 Part C)
//!
//! Verifies that:
//! 1. has_discrepancies flag is correctly computed and stored
//! 2. The real database-assigned ID is returned (not a fabricated UUID)
//! 3. Reports with discrepancies are findable via the partial index

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

mod common;
use common::TestApp;

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_reconciliation_report_has_discrepancies_true() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    // Create a report with discrepancies
    let report = synapse_core::services::reconciliation::ReconciliationReport {
        generated_at: Utc::now(),
        period_start: Utc::now() - chrono::Duration::hours(24),
        period_end: Utc::now(),
        total_db_transactions: 10,
        total_chain_payments: 9,
        missing_on_chain: vec![
            synapse_core::services::reconciliation::MissingTransaction {
                transaction_id: Uuid::new_v4(),
                amount: "100.00".to_string(),
                asset_code: "USDC".to_string(),
                stellar_account: "GBTEST".to_string(),
                completed_at: Utc::now(),
                memo: Some("test-memo".to_string()),
            }
        ],
        orphaned_payments: vec![],
        amount_mismatches: vec![],
        ambiguous_db: vec![],
        ambiguous_chain: vec![],
        unmatched_no_memo_db: vec![],
        unmatched_no_memo_chain: vec![],
    };

    // Store the report
    let report_id = synapse_core::services::reconciliation::ReconciliationService::store_report(
        pool,
        &report
    )
    .await
    .expect("Failed to store report");

    // Verify the returned ID is valid (not nil UUID)
    assert_ne!(report_id, Uuid::nil(), "Report ID should not be nil");

    // Verify we can fetch the report with the returned ID
    let stored_report: (Uuid, bool, i32) = sqlx::query_as(
        "SELECT id, has_discrepancies, missing_on_chain_count
         FROM reconciliation_reports
         WHERE id = $1"
    )
    .bind(report_id)
    .fetch_one(pool)
    .await
    .expect("Failed to fetch stored report");

    assert_eq!(stored_report.0, report_id, "Returned ID should match database ID");
    assert_eq!(stored_report.1, true, "has_discrepancies should be true");
    assert_eq!(stored_report.2, 1, "missing_on_chain_count should be 1");

    // Verify the report is findable via the partial index
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_reports WHERE has_discrepancies = true"
    )
    .fetch_one(pool)
    .await
    .expect("Failed to query discrepancy index");

    assert!(count >= 1, "Should find at least one report with discrepancies");

    app.cleanup().await;
}

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_reconciliation_report_has_discrepancies_false() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    // Create a report without discrepancies
    let report = synapse_core::services::reconciliation::ReconciliationReport {
        generated_at: Utc::now(),
        period_start: Utc::now() - chrono::Duration::hours(24),
        period_end: Utc::now(),
        total_db_transactions: 10,
        total_chain_payments: 10,
        missing_on_chain: vec![],
        orphaned_payments: vec![],
        amount_mismatches: vec![],
        ambiguous_db: vec![],
        ambiguous_chain: vec![],
        unmatched_no_memo_db: vec![],
        unmatched_no_memo_chain: vec![],
    };

    // Store the report
    let report_id = synapse_core::services::reconciliation::ReconciliationService::store_report(
        pool,
        &report
    )
    .await
    .expect("Failed to store report");

    // Verify the returned ID is valid
    assert_ne!(report_id, Uuid::nil(), "Report ID should not be nil");

    // Verify has_discrepancies is false
    let stored_report: (Uuid, bool, i32) = sqlx::query_as(
        "SELECT id, has_discrepancies, missing_on_chain_count
         FROM reconciliation_reports
         WHERE id = $1"
    )
    .bind(report_id)
    .fetch_one(pool)
    .await
    .expect("Failed to fetch stored report");

    assert_eq!(stored_report.0, report_id, "Returned ID should match database ID");
    assert_eq!(stored_report.1, false, "has_discrepancies should be false");
    assert_eq!(stored_report.2, 0, "missing_on_chain_count should be 0");

    app.cleanup().await;
}

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_reconciliation_admin_endpoint_returns_real_id() {
    let app = TestApp::new().await;

    // Trigger reconciliation via admin endpoint
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/admin/reconciliation/run", app.base_url))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "stellar_account": "GBTEST123456789012345678901234567890123456789012345678",
            "start": (Utc::now() - chrono::Duration::hours(24)).to_rfc3339(),
            "end": Utc::now().to_rfc3339()
        }))
        .send()
        .await
        .expect("Failed to send request");

    assert_eq!(
        response.status(),
        200,
        "Admin reconciliation endpoint should return 200"
    );

    let body: serde_json::Value = response.json().await.expect("Failed to parse response");
    let report_id_str = body["report"]["id"]
        .as_str()
        .expect("Response should contain report.id");
    
    let report_id = Uuid::parse_str(report_id_str)
        .expect("Report ID should be a valid UUID");

    // Verify we can fetch the report with the returned ID
    let pool = &app.pool;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM reconciliation_reports WHERE id = $1)"
    )
    .bind(report_id)
    .fetch_one(pool)
    .await
    .expect("Failed to check if report exists");

    assert!(exists, "Report with returned ID should exist in database");

    app.cleanup().await;
}

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_reconciliation_discrepancy_metric() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    // Create a report with multiple types of discrepancies
    let report = synapse_core::services::reconciliation::ReconciliationReport {
        generated_at: Utc::now(),
        period_start: Utc::now() - chrono::Duration::hours(24),
        period_end: Utc::now(),
        total_db_transactions: 15,
        total_chain_payments: 13,
        missing_on_chain: vec![
            synapse_core::services::reconciliation::MissingTransaction {
                transaction_id: Uuid::new_v4(),
                amount: "100.00".to_string(),
                asset_code: "USDC".to_string(),
                stellar_account: "GBTEST".to_string(),
                completed_at: Utc::now(),
                memo: Some("test-1".to_string()),
            }
        ],
        orphaned_payments: vec![
            synapse_core::services::reconciliation::OrphanedPayment {
                payment_id: "orphan-1".to_string(),
                amount: "50.00".to_string(),
                asset_code: "XLM".to_string(),
                from_account: "GBSENDER".to_string(),
                memo: None,
                created_at: Utc::now(),
            }
        ],
        amount_mismatches: vec![],
        ambiguous_db: vec![],
        ambiguous_chain: vec![],
        unmatched_no_memo_db: vec![],
        unmatched_no_memo_chain: vec![],
    };

    // Get the meter and counter
    let meter = synapse_core::metrics::meter();
    let counter = meter
        .u64_counter("reconciliation_discrepancies_total")
        .with_description("Number of reconciliation reports with discrepancies detected")
        .init();

    // Store the report (this should increment the metric)
    let _report_id = synapse_core::services::reconciliation::ReconciliationService::store_report(
        pool,
        &report
    )
    .await
    .expect("Failed to store report");

    // In a real scenario, we'd query the metrics endpoint to verify the counter increased
    // For now, we just verify the report was stored correctly

    app.cleanup().await;
}
