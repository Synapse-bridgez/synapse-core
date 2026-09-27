/// Regression test for the Part A cross-tenant data exposure fix.
///
/// The tracked issue references a test file with this exact name/path,
/// added by an earlier commit ("Closes #947") but never wired into this
/// branch — it does not exist in this checkout (confirmed: `git log` and a
/// full-tree search find no trace of it). This file is written fresh to
/// close that gap and to prove the fix: before this change, all five routes
/// below returned 200 with real, unscoped cross-tenant data to a caller with
/// no credentials at all.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::env;
use synapse_core::{create_app, AppState};
use tower::ServiceExt;
use uuid::Uuid;

fn setup_env() {
    if env::var("DATABASE_URL").is_err() {
        env::set_var(
            "DATABASE_URL",
            "postgres://synapse_app:synapse_app@localhost:5432/synapse_test",
        );
    }
    if env::var("REDIS_URL").is_err() {
        env::set_var("REDIS_URL", "redis://localhost:6379");
    }
}

async fn app() -> axum::Router {
    setup_env();
    let db_url = env::var("DATABASE_URL").unwrap();
    let app_state = AppState::test_new(&db_url).await;
    create_app(app_state)
}

async fn get_unauthenticated(app: &axum::Router, uri: &str) -> StatusCode {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn get_transactions_rejects_unauthenticated_requests() {
    let app = app().await;
    let status = get_unauthenticated(&app, "/transactions").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "GET /transactions must not return 200 to an unauthenticated caller"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn get_transaction_by_id_rejects_unauthenticated_requests() {
    let app = app().await;
    let uri = format!("/transactions/{}", Uuid::new_v4());
    let status = get_unauthenticated(&app, &uri).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "GET /transactions/:id must not return 200 to an unauthenticated caller"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn search_transactions_rejects_unauthenticated_requests() {
    let app = app().await;
    let status = get_unauthenticated(&app, "/transactions/search").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "GET /transactions/search must not return 200 to an unauthenticated caller"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn list_settlements_rejects_unauthenticated_requests() {
    let app = app().await;
    let status = get_unauthenticated(&app, "/settlements").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "GET /settlements must not return 200 to an unauthenticated caller"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn get_settlement_by_id_rejects_unauthenticated_requests() {
    let app = app().await;
    let uri = format!("/settlements/{}", Uuid::new_v4());
    let status = get_unauthenticated(&app, &uri).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "GET /settlements/:id must not return 200 to an unauthenticated caller"
    );
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Same five routes, this time also confirming that a request bearing an
/// invalid (never-issued) API key is rejected just like no key at all —
/// TenantContext must reject unresolvable credentials, not just missing ones.
#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn core_routes_reject_invalid_api_key() {
    let app = app().await;
    for uri in [
        "/transactions".to_string(),
        format!("/transactions/{}", Uuid::new_v4()),
        "/transactions/search".to_string(),
        "/settlements".to_string(),
        format!("/settlements/{}", Uuid::new_v4()),
    ] {
        let status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri.as_str())
                    .header("X-API-Key", "this-key-was-never-issued")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status();
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} must reject an invalid API key"
        );
    }
}
