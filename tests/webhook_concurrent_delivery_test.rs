use mockito::Server;
use redis::Client as RedisClient;
use sqlx::migrate::Migrator;
use sqlx::{PgPool, Row};
use std::path::Path;
use synapse_core::services::WebhookDispatcher;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// Set up a test Postgres instance with all migrations applied.
/// Also ensures the current month partition exists for the transactions table.
async fn setup_postgres() -> (PgPool, ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let pool = PgPool::connect(&url).await.unwrap();

    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();

    // Ensure current month partition for transactions table
    let _ = sqlx::query(
        r#"
        DO $$
        DECLARE
            p_date DATE := DATE_TRUNC('month', NOW());
            p_name TEXT := 'transactions_y' || TO_CHAR(p_date, 'YYYY') || 'm' || TO_CHAR(p_date, 'MM');
        BEGIN
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = p_name) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    p_name,
                    TO_CHAR(p_date, 'YYYY-MM-DD'),
                    TO_CHAR(p_date + INTERVAL '1 month', 'YYYY-MM-DD')
                );
            END IF;
        END $$;
        "#,
    )
    .execute(&pool)
    .await;

    (pool, container)
}

/// Set up Redis – tries testcontainers first, falls back to REDIS_URL env var.
async fn setup_redis() -> (String, Option<ContainerAsync<testcontainers::GenericImage>>) {
    // Prefer a running Redis from the environment.
    if let Ok(url) = std::env::var("REDIS_URL") {
        return (url, None);
    }
    // Fallback: start a Redis container with testcontainers.
    let image = testcontainers::GenericImage::new("redis", "7-alpine");
    let container = image.start().await.unwrap();
    let port = container.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://127.0.0.1:{}/", port);
    (url, Some(container))
}

/// Helper: insert a webhook endpoint and a pending delivery row.
async fn insert_endpoint_and_delivery(
    pool: &PgPool,
    url: &str,
    max_delivery_rate: i32,
    event_type: &str,
) -> (Uuid, Uuid) {
    let endpoint_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO webhook_endpoints (url, secret, event_types, max_delivery_rate)
        VALUES ($1, 'test-secret', ARRAY[$3], $2)
        RETURNING id
        "#,
    )
    .bind(url)
    .bind(max_delivery_rate)
    .bind(event_type)
    .fetch_one(pool)
    .await
    .expect("Failed to insert endpoint");

    let delivery_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO webhook_deliveries
            (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
        VALUES ($1, $2, $3, $4, 'pending', NOW())
        RETURNING id
        "#,
    )
    .bind(endpoint_id)
    .bind(Uuid::new_v4())
    .bind(event_type)
    .bind(serde_json::json!({"event_type": event_type, "transaction_id": "test", "timestamp": "2025-01-01T00:00:00Z", "data": {"key": "value"}}))
    .fetch_one(pool)
    .await
    .expect("Failed to insert delivery");

    (endpoint_id, delivery_id)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 1: Two concurrent process_pending runs deliver each event exactly once
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_process_pending_delivers_exactly_once() {
    let (pool, _pg) = setup_postgres().await;
    let (redis_url, _redis) = setup_redis().await;

    // Start a mock server that accepts POSTs and returns 200.
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/webhook")
        .with_status(200)
        .expect(1) // Exactly one delivery expected
        .create();

    let endpoint_url = format!("{}/webhook", server.url());

    // Insert one endpoint + one delivery.
    let (_ep_id, delivery_id) =
        insert_endpoint_and_delivery(&pool, &endpoint_url, 100, "test.event").await;

    // Create two independent dispatcher instances (simulating two replicas).
    let dispatcher1 =
        WebhookDispatcher::new(pool.clone(), &redis_url).expect("dispatcher 1");
    let dispatcher2 =
        WebhookDispatcher::new(pool.clone(), &redis_url).expect("dispatcher 2");

    // Run both process_pending calls concurrently.
    let (r1, r2) = tokio::join!(dispatcher1.process_pending(), dispatcher2.process_pending());
    assert!(r1.is_ok(), "first process_pending should succeed: {:?}", r1);
    assert!(r2.is_ok(), "second process_pending should succeed: {:?}", r2);

    // The mock was set to expect(1), so mockito will assert that only one
    // request was received.
    mock.assert_async().await;

    // Verify the delivery row was marked as 'delivered'.
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_one(&pool)
        .await
        .expect("delivery should exist");
    assert_eq!(status, "delivered", "delivery should be delivered exactly once");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 2: An exhausted delivery lands in the DLQ with full attempt history
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_exhausted_delivery_routed_to_dlq_with_history() {
    let (pool, _pg) = setup_postgres().await;
    let (redis_url, _redis) = setup_redis().await;

    // Mock endpoint always returns 500 so the delivery exhausts.
    let mut server = Server::new_async().await;
    let mock = server
        .mock("POST", "/fail")
        .with_status(500)
        .expect(5) // Exactly MAX_ATTEMPTS (5) attempts
        .create();

    let endpoint_url = format!("{}/fail", server.url());
    let (_ep_id, delivery_id) =
        insert_endpoint_and_delivery(&pool, &endpoint_url, 100, "test.exhaust").await;

    let dispatcher =
        WebhookDispatcher::new(pool.clone(), &redis_url).expect("dispatcher");

    // Run process_pending in a loop to exhaust the delivery.
    // After each failure, reset next_attempt_at so the next cycle picks it up
    // immediately, bypassing the exponential backoff.
    for _ in 0..6 {
        let _ = dispatcher.process_pending().await;
        sqlx::query(
            "UPDATE webhook_deliveries SET next_attempt_at = NOW() WHERE status = 'pending'",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    mock.assert_async().await;

    // Verify the delivery is now 'failed'.
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_deliveries WHERE id = $1")
        .bind(delivery_id)
        .fetch_one(&pool)
        .await
        .expect("delivery should exist");
    assert_eq!(status, "failed", "delivery should be failed after exhaustion");

    // Verify the DLQ entry exists.
    let dlq_entry: Option<(Uuid, i32, serde_json::Value)> = sqlx::query_as(
        r#"
        SELECT id, attempt_count, attempt_history
        FROM webhook_delivery_dlq
        WHERE delivery_id = $1
        "#,
    )
    .bind(delivery_id)
    .fetch_optional(&pool)
    .await
    .expect("DLQ query should succeed");

    assert!(
        dlq_entry.is_some(),
        "Delivery should be in the DLQ after exhaustion"
    );

    let (_dlq_id, attempt_count, attempt_history) = dlq_entry.unwrap();
    assert_eq!(attempt_count, 5, "DLQ should record 5 attempts");

    // attempt_history should be a JSON array with 5 entries
    if let Some(history_array) = attempt_history.as_array() {
        assert!(
            history_array.len() >= 5,
            "DLQ attempt history should have at least 5 entries, got {}",
            history_array.len()
        );
        // Each entry should have an attempt field
        for entry in history_array {
            assert!(
                entry.get("attempt").is_some(),
                "Each history entry should have an 'attempt' field"
            );
        }
    } else {
        panic!("attempt_history should be a JSON array, got {:?}", attempt_history);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test 3: A failing endpoint trips its breaker without starving healthy endpoints
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_circuit_breaker_isolates_failing_endpoint() {
    let (pool, _pg) = setup_postgres().await;
    let (redis_url, _redis) = setup_redis().await;

    // Two separate mock servers: one that always fails, one that succeeds.
    let mut fail_server = Server::new_async().await;
    let _failing_mock = fail_server
        .mock("POST", "/fail-endpoint")
        .with_status(500)
        .create(); // No fixed expect count – it will trip the breaker.

    let mut ok_server = Server::new_async().await;
    let _healthy_mock = ok_server
        .mock("POST", "/healthy-endpoint")
        .with_status(200)
        .create();

    let fail_url = format!("{}/fail-endpoint", fail_server.url());
    let ok_url = format!("{}/healthy-endpoint", ok_server.url());

    // Insert two endpoints: one failing, one healthy.
    let failing_ep_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO webhook_endpoints (url, secret, event_types, max_delivery_rate)
        VALUES ($1, 'fail-secret', ARRAY['fail.event'], 100)
        RETURNING id
        "#,
    )
    .bind(&fail_url)
    .fetch_one(&pool)
    .await
    .expect("Failed to insert failing endpoint");

    let healthy_ep_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO webhook_endpoints (url, secret, event_types, max_delivery_rate)
        VALUES ($1, 'ok-secret', ARRAY['ok.event'], 100)
        RETURNING id
        "#,
    )
    .bind(&ok_url)
    .fetch_one(&pool)
    .await
    .expect("Failed to insert healthy endpoint");

    // Insert 3 deliveries for the failing endpoint to trip the breaker.
    let mut failing_delivery_ids = Vec::new();
    for _ in 0..3 {
        let did: Uuid = sqlx::query_scalar(
            r#"
            INSERT INTO webhook_deliveries
                (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
            VALUES ($1, $2, 'fail.event', $3, 'pending', NOW())
            RETURNING id
            "#,
        )
        .bind(failing_ep_id)
        .bind(Uuid::new_v4())
        .bind(serde_json::json!({"event_type": "fail.event", "transaction_id": "fail", "timestamp": "2025-01-01T00:00:00Z", "data": {}}))
        .fetch_one(&pool)
        .await
        .expect("Failed to insert failing delivery");
        failing_delivery_ids.push(did);
    }

    // Insert 1 delivery for the healthy endpoint.
    let healthy_delivery_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO webhook_deliveries
            (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
        VALUES ($1, $2, 'ok.event', $3, 'pending', NOW())
        RETURNING id
        "#,
    )
    .bind(healthy_ep_id)
    .bind(Uuid::new_v4())
    .bind(serde_json::json!({"event_type": "ok.event", "transaction_id": "ok", "timestamp": "2025-01-01T00:00:00Z", "data": {}}))
    .fetch_one(&pool)
    .await
    .expect("Failed to insert healthy delivery");

    let dispatcher =
        WebhookDispatcher::new(pool.clone(), &redis_url).expect("dispatcher");

    // Run process_pending multiple times, resetting next_attempt_at so the
    // failures happen back-to-back (bypassing exponential backoff).
    // The failing endpoint should trip the circuit breaker after
    // CB_FAILURE_THRESHOLD (3) failures.
    // The healthy endpoint should always be delivered.
    for _ in 0..6 {
        let _ = dispatcher.process_pending().await;
        sqlx::query(
            "UPDATE webhook_deliveries SET next_attempt_at = NOW() WHERE status = 'pending'",
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // All failing deliveries should still be pending (not burned), but their
    // next_attempt_at should be pushed into the future by the circuit breaker.
    for did in &failing_delivery_ids {
        let row = sqlx::query(
            "SELECT status, attempt_count, next_attempt_at FROM webhook_deliveries WHERE id = $1",
        )
        .bind(did)
        .fetch_one(&pool)
        .await
        .expect("failing delivery should exist");

        let status: String = row.get("status");
        let attempt_count: i32 = row.get("attempt_count");

        assert_eq!(
            status, "pending",
            "failing delivery {} should still be pending (not consumed)",
            did
        );
        assert!(
            attempt_count < 5,
            "failing delivery {} should not have exhausted attempts (was {})",
            did,
            attempt_count
        );
    }

    // The healthy delivery should have been delivered.
    let healthy_status: String =
        sqlx::query_scalar("SELECT status FROM webhook_deliveries WHERE id = $1")
            .bind(healthy_delivery_id)
            .fetch_one(&pool)
            .await
            .expect("healthy delivery should exist");
    assert_eq!(
        healthy_status, "delivered",
        "healthy endpoint delivery should succeed despite failing endpoint"
    );

    // Verify the circuit breaker key exists in Redis.
    let mut redis_conn = RedisClient::open(redis_url.as_str())
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let cb_key = format!("webhook_cb:{failing_ep_id}");
    let cb_data: Option<String> = redis::cmd("GET")
        .arg(&cb_key)
        .query_async(&mut redis_conn)
        .await
        .unwrap();
    assert!(
        cb_data.is_some(),
        "Circuit breaker state should be persisted in Redis for failing endpoint"
    );
    if let Some(data) = cb_data {
        let state: serde_json::Value = serde_json::from_str(&data).unwrap();
        assert_eq!(state["state"], "open", "Circuit breaker should be open");
    }
}
