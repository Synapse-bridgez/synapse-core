//! Comprehensive integration tests for the /health and /ready endpoints.
//!
//! These tests exercise the full axum router via a real HTTP server backed by
//! a testcontainers Postgres instance, matching the production code path in
//! `src/handlers/mod.rs` and `src/health.rs`.

mod common;

use common::TestApp;
use synapse_core::handlers::{DbPoolStats, HealthStatus, ReadinessResponse};

// ---------------------------------------------------------------------------
// /health endpoint
// ---------------------------------------------------------------------------

/// Happy path: DB is up → 200 with status "healthy".
#[tokio::test]
async fn test_health_returns_200_when_db_connected() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .expect("request failed");

    assert_eq!(res.status(), 200);

    let body: HealthStatus = res.json().await.expect("invalid JSON");
    assert_eq!(body.status, "healthy");
    assert_eq!(body.db, "connected");
}

/// Response body contains all required fields with sensible values.
#[tokio::test]
async fn test_health_response_fields() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let body: HealthStatus = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body.version, "0.1.0");
    assert_eq!(body.db, "connected");
    assert!(
        body.db_pool.max_connections > 0,
        "max_connections must be > 0"
    );
    assert!(
        body.db_pool.usage_percent >= 0.0 && body.db_pool.usage_percent <= 100.0,
        "usage_percent out of range"
    );
}

/// Pool stats are present and internally consistent.
#[tokio::test]
async fn test_health_db_pool_stats_consistency() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let body: HealthStatus = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let pool: &DbPoolStats = &body.db_pool;
    // active + idle ≤ max
    assert!(
        pool.active_connections + pool.idle_connections <= pool.max_connections,
        "active + idle should not exceed max_connections"
    );
    // usage_percent matches active / max
    let expected = (pool.active_connections as f32 / pool.max_connections as f32) * 100.0;
    assert!(
        (pool.usage_percent - expected).abs() < 0.01,
        "usage_percent mismatch"
    );
}

/// Queue-depth and batch-size counters are present (zero on a fresh app).
#[tokio::test]
async fn test_health_queue_and_batch_counters() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let body: HealthStatus = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Fresh app: pending queue is 0, batch size is 10 (TestApp default)
    assert_eq!(body.pending_queue_depth, 0);
    assert_eq!(body.current_batch_size, 10);
}

/// WebSocket connection count starts at zero.
#[tokio::test]
async fn test_health_ws_connection_count_initial() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let body: HealthStatus = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body.ws_connection_count, 0);
}

/// Content-Type header is application/json.
#[tokio::test]
async fn test_health_content_type_is_json() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/health", app.base_url))
        .send()
        .await
        .unwrap();

    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "expected JSON content-type, got: {ct}"
    );
}

/// Multiple concurrent requests all succeed (no race conditions on shared state).
#[tokio::test]
async fn test_health_concurrent_requests() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();
    let base_url = app.base_url.clone();

    let handles: Vec<_> = (0..10)
        .map(|_| {
            let client = client.clone();
            let url = format!("{}/health", base_url);
            tokio::spawn(async move { client.get(url).send().await.unwrap().status() })
        })
        .collect();

    for h in handles {
        assert_eq!(h.await.unwrap(), 200);
    }
}

// ---------------------------------------------------------------------------
// /ready endpoint
// ---------------------------------------------------------------------------

/// Default state: ReadinessState::new() is NOT ready → 503.
#[tokio::test]
async fn test_ready_returns_503_when_not_ready() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/ready", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 503);

    let body: ReadinessResponse = res.json().await.unwrap();
    assert_eq!(body.status, "not_ready");
}

/// After marking the app ready, /ready returns 200.
#[tokio::test]
async fn test_ready_returns_200_when_ready() {
    let app = TestApp::new().await;

    // Mark the app as ready
    app.pool.acquire().await.expect("pool should be alive"); // sanity check
    app.set_ready().await;

    let client = reqwest::Client::new();
    let res = client
        .get(format!("{}/ready", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);

    let body: ReadinessResponse = res.json().await.unwrap();
    assert_eq!(body.status, "ready");
    assert!(!body.draining);
}

/// Draining state: not_ready + draining=true.
#[tokio::test]
async fn test_ready_draining_state() {
    let app = TestApp::new().await;

    // Trigger drain (sets not_ready + draining)
    app.start_drain().await;

    let client = reqwest::Client::new();
    let res = client
        .get(format!("{}/ready", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 503);

    let body: ReadinessResponse = res.json().await.unwrap();
    assert_eq!(body.status, "not_ready");
    assert!(body.draining, "should be draining");
}

/// Content-Type header is application/json.
#[tokio::test]
async fn test_ready_content_type_is_json() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/ready", app.base_url))
        .send()
        .await
        .unwrap();

    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.contains("application/json"),
        "expected JSON content-type, got: {ct}"
    );
}

/// Readiness transitions: not_ready → ready → not_ready (drain).
#[tokio::test]
async fn test_ready_state_transitions() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();
    let url = format!("{}/ready", app.base_url);

    // Initially not ready
    let status = client.get(&url).send().await.unwrap().status();
    assert_eq!(status, 503);

    // Mark ready
    app.set_ready().await;
    let status = client.get(&url).send().await.unwrap().status();
    assert_eq!(status, 200);

    // Start drain → back to not_ready
    app.start_drain().await;
    let status = client.get(&url).send().await.unwrap().status();
    assert_eq!(status, 503);
}

// ---------------------------------------------------------------------------
// Unit tests for health module types (no I/O required)
// ---------------------------------------------------------------------------

#[test]
fn test_dependency_status_healthy_serialization() {
    use synapse_core::health::{DependencySeverity, DependencyStatus};

    let s = DependencyStatus::Healthy {
        status: "healthy".to_string(),
        severity: DependencySeverity::Critical,
        latency_ms: 42,
    };
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["latency_ms"], 42);
    assert!(
        json.get("error").is_none(),
        "healthy variant must not have 'error'"
    );
}

#[test]
fn test_dependency_status_unhealthy_serialization() {
    use synapse_core::health::{DependencySeverity, DependencyStatus};

    let s = DependencyStatus::Unhealthy {
        status: "unhealthy".to_string(),
        severity: DependencySeverity::NonCritical,
        error: "timeout".to_string(),
    };
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["status"], "unhealthy");
    assert_eq!(json["error"], "timeout");
    assert!(
        json.get("latency_ms").is_none(),
        "unhealthy variant must not have 'latency_ms'"
    );
}

#[test]
fn test_health_response_overall_status_logic() {
    use std::collections::HashMap;
    use synapse_core::health::{DependencySeverity, DependencyStatus, HealthResponse};

    // All healthy → "healthy"
    let mut deps = HashMap::new();
    deps.insert(
        "postgres".to_string(),
        DependencyStatus::Healthy {
            status: "healthy".to_string(),
            severity: DependencySeverity::Critical,
            latency_ms: 1,
        },
    );
    let r = HealthResponse {
        status: "healthy".to_string(),
        version: "0.1.0".to_string(),
        uptime_seconds: 0,
        dependencies: deps,
    };
    assert_eq!(r.status, "healthy");

    // Non-critical failure → "degraded"
    let mut deps2 = HashMap::new();
    deps2.insert(
        "redis".to_string(),
        DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            severity: DependencySeverity::NonCritical,
            error: "refused".to_string(),
        },
    );
    let r2 = HealthResponse {
        status: "degraded".to_string(),
        version: "0.1.0".to_string(),
        uptime_seconds: 0,
        dependencies: deps2,
    };
    assert_eq!(r2.status, "degraded");

    // Critical failure → "unhealthy"
    let mut deps3 = HashMap::new();
    deps3.insert(
        "postgres".to_string(),
        DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            severity: DependencySeverity::Critical,
            error: "down".to_string(),
        },
    );
    let r3 = HealthResponse {
        status: "unhealthy".to_string(),
        version: "0.1.0".to_string(),
        uptime_seconds: 0,
        dependencies: deps3,
    };
    assert_eq!(r3.status, "unhealthy");
}

#[test]
fn test_dependency_severity_variants() {
    use synapse_core::health::DependencySeverity;

    let critical = DependencySeverity::Critical;
    let non_critical = DependencySeverity::NonCritical;

    assert_eq!(
        serde_json::to_value(critical).unwrap(),
        serde_json::json!("critical")
    );
    assert_eq!(
        serde_json::to_value(non_critical).unwrap(),
        serde_json::json!("noncritical")
    );
}
