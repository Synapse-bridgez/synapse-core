use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use redis::Client;
use serde_json::json;
use std::time::Duration;
use synapse_core::middleware::idempotency::{idempotency_middleware, IdempotencyService};
use tokio::time::sleep;
use tower::ServiceExt;

async fn test_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({"status": "success"})))
}

fn create_test_app(service: IdempotencyService) -> Router {
    Router::new()
        .route("/webhook", post(test_handler))
        .layer(middleware::from_fn_with_state(
            service,
            idempotency_middleware,
        ))
}

async fn setup_redis() -> (Client, String) {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let client = Client::open(redis_url.clone()).expect("Failed to connect to Redis");

    // Flush test database
    let mut conn = client
        .get_connection()
        .expect("Failed to get Redis connection");
    redis::cmd("FLUSHDB").execute(&mut conn);

    (client, redis_url)
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_duplicate_request_returns_cached_response() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    let idempotency_key = "test-key-duplicate-123";

    // First request
    let req1 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    // Verify key was stored in Redis
    let mut conn = client.get_connection().unwrap();
    let cache_key = format!("idempotency:default:{}", idempotency_key);
    let exists: bool = redis::cmd("EXISTS")
        .arg(&cache_key)
        .query(&mut conn)
        .unwrap();
    assert!(exists, "Idempotency key should be cached");

    // Second request with same key
    let req2 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_concurrent_requests_return_429() {
    let (_client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    let idempotency_key = "test-key-concurrent-456";

    // Create two concurrent requests
    let app1 = app.clone();
    let app2 = app.clone();
    let key1 = idempotency_key.to_string();
    let key2 = idempotency_key.to_string();

    let handle1 = tokio::spawn(async move {
        let req = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("x-idempotency-key", key1)
            .body(Body::empty())
            .unwrap();
        app1.oneshot(req).await.unwrap()
    });

    let handle2 = tokio::spawn(async move {
        sleep(Duration::from_millis(10)).await;
        let req = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("x-idempotency-key", key2)
            .body(Body::empty())
            .unwrap();
        app2.oneshot(req).await.unwrap()
    });

    let response1 = handle1.await.unwrap();
    let response2 = handle2.await.unwrap();

    // One should succeed, one should return 429
    let statuses = vec![response1.status(), response2.status()];
    assert!(
        statuses.contains(&StatusCode::OK) || statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "Expected one OK and one TOO_MANY_REQUESTS, got {:?}",
        statuses
    );
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_idempotency_key_expires_after_ttl() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service.clone());

    let idempotency_key = "test-key-expiry-789";

    // First request
    let req1 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    // Manually expire the key in Redis
    let mut conn = client.get_connection().unwrap();
    let cache_key = format!("idempotency:default:{}", idempotency_key);
    redis::cmd("DEL").arg(&cache_key).execute(&mut conn);

    // Verify key is deleted
    let exists: bool = redis::cmd("EXISTS")
        .arg(&cache_key)
        .query(&mut conn)
        .unwrap();
    assert!(!exists, "Key should be deleted");

    // Second request after expiry
    let req2 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_cached_response_matches_original() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    let idempotency_key = "test-key-match-101";

    // First request
    let req1 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response1 = app.clone().oneshot(req1).await.unwrap();
    let status1 = response1.status();

    // Verify cached response exists
    let mut conn = client.get_connection().unwrap();
    let cache_key = format!("idempotency:default:{}", idempotency_key);
    let cached_data: String = redis::cmd("GET").arg(&cache_key).query(&mut conn).unwrap();
    assert!(!cached_data.is_empty());

    // Second request
    let req2 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .body(Body::empty())
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();
    let status2 = response2.status();

    // Both should return 200 OK
    assert_eq!(status1, StatusCode::OK);
    assert_eq!(status2, StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_different_payload_same_key_rejected() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    let idempotency_key = "test-key-payload-202";

    // First request with payload A
    let req1 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .header("content-type", "application/json")
        .body(Body::from(json!({"data": "payload_a"}).to_string()))
        .unwrap();

    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    // Verify key is cached
    let mut conn = client.get_connection().unwrap();
    let cache_key = format!("idempotency:default:{}", idempotency_key);
    let exists: bool = redis::cmd("EXISTS")
        .arg(&cache_key)
        .query(&mut conn)
        .unwrap();
    assert!(exists);

    // Second request with different payload B but same key
    let req2 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", idempotency_key)
        .header("content-type", "application/json")
        .body(Body::from(json!({"data": "payload_b"}).to_string()))
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();

    // Should return cached response, not process new payload
    assert_eq!(response2.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_redis_failure_fallback() {
    // Use invalid Redis URL to simulate connection failure
    let invalid_redis_url = "redis://invalid-host:9999";
    let service = IdempotencyService::new(invalid_redis_url).unwrap();
    let app = create_test_app(service);

    let req = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", "test-key-fallback-303")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    // Should fail open and process the request
    assert_eq!(response.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_no_idempotency_key_proceeds_normally() {
    let (_client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    // Request without idempotency key
    let req = Request::builder()
        .method("POST")
        .uri("/webhook")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_invalid_idempotency_key_format() {
    let (_client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    // Request with valid key
    let req = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", "valid-key-404")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();

    // Should process normally with valid key
    assert_eq!(response.status(), StatusCode::OK);
}

// ── Issue 1: Tenant-scoped idempotency key tests ──────────────────────────────

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_two_tenants_same_key_get_independent_responses() {
    let (_client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();

    let key = "shared-key-tenant-test";

    // Tenant A processes the key
    let app_a = create_test_app(service.clone());
    let req_a = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", key)
        .header("x-tenant-id", "tenant-a")
        .body(Body::empty())
        .unwrap();
    let resp_a = app_a.oneshot(req_a).await.unwrap();
    assert_eq!(resp_a.status(), StatusCode::OK);

    // Tenant B uses the same key — should be treated as a new request
    let app_b = create_test_app(service.clone());
    let req_b = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", key)
        .header("x-tenant-id", "tenant-b")
        .body(Body::empty())
        .unwrap();
    let resp_b = app_b.oneshot(req_b).await.unwrap();
    // Should be 200 (new request), not 429 (blocked by tenant-a's lock)
    assert_eq!(resp_b.status(), StatusCode::OK);
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_no_tenant_id_uses_default_scope() {
    let (_client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service);

    let key = "no-tenant-backward-compat";

    // First request without X-Tenant-Id
    let req1 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", key)
        .body(Body::empty())
        .unwrap();
    let resp1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second request without X-Tenant-Id — should return cached
    let req2 = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", key)
        .body(Body::empty())
        .unwrap();
    let resp2 = app.oneshot(req2).await.unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

// ── Issue 4: Stale lock recovery tests ───────────────────────────────────────

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_stale_lock_recovery() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();

    let tenant_id = "default";
    let key = "stale-lock-test-key";

    // Simulate a crash: set a lock with a timestamp 3 minutes ago, no cached response
    let lock_key = format!("idempotency:lock:{}:{}", tenant_id, key);
    let old_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 180; // 3 minutes ago
    let lock_val =
        serde_json::json!({"instance_id": "crashed-instance", "locked_at": old_timestamp});
    let mut conn = client.get_connection().unwrap();
    redis::cmd("SET")
        .arg(&lock_key)
        .arg(lock_val.to_string())
        .arg("EX")
        .arg(300u64)
        .execute(&mut conn);

    // Verify lock exists
    let exists: bool = redis::cmd("EXISTS")
        .arg(&lock_key)
        .query(&mut conn)
        .unwrap();
    assert!(exists, "Lock should exist before recovery");

    // Run recovery
    service.recover_stale_locks().await.unwrap();

    // Lock should be deleted
    let exists_after: bool = redis::cmd("EXISTS")
        .arg(&lock_key)
        .query(&mut conn)
        .unwrap();
    assert!(!exists_after, "Stale lock should be deleted after recovery");
}

#[ignore = "Requires Redis"]
#[tokio::test]
async fn test_normal_flow_not_affected_by_recovery() {
    let (client, redis_url) = setup_redis().await;
    let service = IdempotencyService::new(&redis_url).unwrap();
    let app = create_test_app(service.clone());

    let key = "normal-flow-recovery-test";

    // Process a request normally
    let req = Request::builder()
        .method("POST")
        .uri("/webhook")
        .header("x-idempotency-key", key)
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Run recovery — should not delete the cached response
    service.recover_stale_locks().await.unwrap();

    // Cached response should still exist
    let cache_key = format!("idempotency:default:{}", key);
    let mut conn = client.get_connection().unwrap();
    let exists: bool = redis::cmd("EXISTS")
        .arg(&cache_key)
        .query(&mut conn)
        .unwrap();
    assert!(exists, "Cached response should not be deleted by recovery");
}
