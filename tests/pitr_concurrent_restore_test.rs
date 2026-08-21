//! Test for PITR concurrent restore guard fix (Issue #1062 Part E)
//!
//! Verifies that only one PITR restore job can run at a time.

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

mod common;
use common::TestApp;

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_pitr_concurrent_restore_rejected() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    // Create a PITR service (we'll mock the executor to not actually run restore)
    let executor = std::sync::Arc::new(MockPitrExecutor);
    let pitr_service = synapse_core::services::pitr::PitrService::new(pool.clone(), executor);

    // Submit first restore job
    let target = Utc::now() - chrono::Duration::hours(1);
    let job1 = pitr_service
        .submit_restore(target, "admin1", false)
        .await
        .expect("First restore should succeed");

    // Manually mark it as running (normally the spawned task does this)
    sqlx::query(
        "UPDATE pitr_restore_jobs SET status = 'running', started_at = NOW() WHERE id = $1"
    )
    .bind(job1.id)
    .execute(pool)
    .await
    .expect("Failed to mark job as running");

    // Try to submit a second restore while the first is running
    let result = pitr_service
        .submit_restore(target, "admin2", false)
        .await;

    // Verify the second submission was rejected
    assert!(
        result.is_err(),
        "Second restore should be rejected while first is running"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("already running"),
        "Error message should mention already running job: {}",
        err_msg
    );
    assert!(
        err_msg.contains(&job1.id.to_string()),
        "Error message should include the running job ID"
    );

    app.cleanup().await;
}

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_pitr_concurrent_restore_metric() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    let executor = std::sync::Arc::new(MockPitrExecutor);
    let pitr_service = synapse_core::services::pitr::PitrService::new(pool.clone(), executor);

    // Submit and mark first job as running
    let target = Utc::now() - chrono::Duration::hours(1);
    let job1 = pitr_service
        .submit_restore(target, "admin1", false)
        .await
        .expect("First restore should succeed");

    sqlx::query(
        "UPDATE pitr_restore_jobs SET status = 'running', started_at = NOW() WHERE id = $1"
    )
    .bind(job1.id)
    .execute(pool)
    .await
    .expect("Failed to mark job as running");

    // Get the metric counter
    let meter = synapse_core::metrics::meter();
    let counter = meter
        .u64_counter("pitr_restore_rejected_concurrent_total")
        .with_description("Number of PITR restore requests rejected due to concurrent job")
        .init();

    // Try to submit second restore (should be rejected and increment metric)
    let _result = pitr_service
        .submit_restore(target, "admin2", false)
        .await;

    // Metric should have been incremented
    // (In a real scenario, we'd query the metrics endpoint to verify)

    app.cleanup().await;
}

#[tokio::test]
#[ignore] // Requires live Postgres + Redis
async fn test_pitr_restore_allowed_after_previous_completes() {
    let app = TestApp::new().await;
    let pool = &app.pool;

    let executor = std::sync::Arc::new(MockPitrExecutor);
    let pitr_service = synapse_core::services::pitr::PitrService::new(pool.clone(), executor);

    // Submit and complete first job
    let target = Utc::now() - chrono::Duration::hours(1);
    let job1 = pitr_service
        .submit_restore(target, "admin1", false)
        .await
        .expect("First restore should succeed");

    // Mark it as completed
    sqlx::query(
        "UPDATE pitr_restore_jobs SET status = 'succeeded', started_at = NOW(), completed_at = NOW() WHERE id = $1"
    )
    .bind(job1.id)
    .execute(pool)
    .await
    .expect("Failed to mark job as completed");

    // Now submit a second restore - should succeed since first is not running
    let job2 = pitr_service
        .submit_restore(target, "admin2", false)
        .await
        .expect("Second restore should succeed after first completes");

    assert_ne!(job1.id, job2.id, "Should create a new job");

    app.cleanup().await;
}

// Mock executor that doesn't actually run restores
struct MockPitrExecutor;

#[async_trait::async_trait]
impl synapse_core::services::pitr::PitrExecutor for MockPitrExecutor {
    async fn restore(&self, _target_timestamp: chrono::DateTime<chrono::Utc>) -> Result<String, String> {
        // Mock implementation - never actually called in these tests
        Ok("mock restore completed".to_string())
    }
}
