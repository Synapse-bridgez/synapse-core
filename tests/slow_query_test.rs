//! Slow Query Logger & EXPLAIN Plan Capture Unit and Integration Test Suite.

use synapse_core::db::slow_query::{SlowQueryConfig, SlowQueryLogger};

fn get_database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[tokio::test]
async fn test_slow_query_threshold_filter() {
    let config = SlowQueryConfig {
        threshold_ms: 100,
        max_stored_queries: 10,
        max_plan_bytes: 4096,
        auto_explain_enabled: true,
        capture_explain: true,
    };

    let logger = SlowQueryLogger::new(config);

    // Fast query (50ms) - should NOT be recorded
    let res1 = logger.record_query(None, "SELECT 1", 50).await;
    assert!(res1.is_none());
    assert_eq!(logger.get_slow_queries().await.len(), 0);

    // Slow query (150ms) - SHOULD be recorded
    let res2 = logger.record_query(None, "SELECT 2", 150).await;
    assert!(res2.is_some());
    let record = res2.unwrap();
    assert_eq!(record.duration_ms, 150);
    assert_eq!(record.query_text, "SELECT 2");
    assert_eq!(logger.get_slow_queries().await.len(), 1);
}

#[tokio::test]
async fn test_write_query_safety_guard() {
    let logger = SlowQueryLogger::new(SlowQueryConfig::with_threshold(50));

    // Verify write query detection
    assert!(SlowQueryLogger::is_write_query("INSERT INTO users (id) VALUES ('1')"));
    assert!(SlowQueryLogger::is_write_query("UPDATE transactions SET status = 'completed'"));
    assert!(SlowQueryLogger::is_write_query("DELETE FROM webhook_deliveries"));
    assert!(!SlowQueryLogger::is_write_query("SELECT * FROM transactions"));

    // Record slow write query - should set safety flag and bypass re-execution
    let rec = logger
        .record_query(None, "UPDATE transactions SET status = 'completed'", 200)
        .await
        .unwrap();

    assert!(rec.is_write_query);
    assert!(rec.explain_plan.is_some());
    assert!(rec
        .explain_plan
        .unwrap()
        .contains("EXPLAIN ANALYZE re-execution bypassed for safety"));
}

#[tokio::test]
async fn test_slow_query_explain_plan_capture_with_db() {
    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping DB EXPLAIN capture test: DATABASE_URL not set");
            return;
        }
    };

    let pool = match sqlx::PgPool::connect(&database_url).await {
        Ok(p) => p,
        Err(e) => {
            println!("Skipping DB test (connect failed): {:?}", e);
            return;
        }
    };

    let logger = SlowQueryLogger::new(SlowQueryConfig::with_threshold(50));

    // Execute EXPLAIN capture on read query
    let plan = logger
        .capture_explain_plan(&pool, "SELECT 1 as test_val")
        .await;

    assert!(plan.is_some());
    let plan_text = plan.unwrap();
    println!("Captured EXPLAIN plan:\n{}", plan_text);
    assert!(plan_text.contains("Result") || plan_text.contains("Planning Time") || plan_text.contains("Execution Time"));

    // Trigger slow query with pg_sleep
    let rec = logger
        .record_query(Some(&pool), "SELECT pg_sleep(0.06)", 60)
        .await;

    assert!(rec.is_some());
    let record = rec.unwrap();
    assert!(record.explain_plan.is_some());
    println!("Slow query record captured:\n{:?}", record);
}

#[tokio::test]
async fn test_bounded_retention_and_payload_truncation() {
    let config = SlowQueryConfig {
        threshold_ms: 10,
        max_stored_queries: 3,
        max_plan_bytes: 50,
        auto_explain_enabled: false,
        capture_explain: true,
    };

    let logger = SlowQueryLogger::new(config);

    // Record 5 slow queries (max_stored_queries is 3)
    for i in 1..=5 {
        logger
            .record_query(None, &format!("SELECT {}", i), 100)
            .await;
    }

    let records = logger.get_slow_queries().await;
    assert_eq!(records.len(), 3, "Ring buffer must strictly hold max 3 records");
    assert_eq!(records[0].query_text, "SELECT 3");
    assert_eq!(records[1].query_text, "SELECT 4");
    assert_eq!(records[2].query_text, "SELECT 5");

    // Test plan payload truncation limit
    let long_plan_query = logger
        .record_query(None, "UPDATE dummy SET col = 1", 100)
        .await
        .unwrap();

    let plan = long_plan_query.explain_plan.unwrap();
    assert!(plan.contains("truncated at max_plan_bytes limit"));
    assert!(plan.len() <= 120);
}
