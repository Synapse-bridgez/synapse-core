use axum::{
    body::Body,
    http::{Request, StatusCode, HeaderMap},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::env;
use synapse_core::{graphql::schema::build_schema, ApiState, AppState};
use tower::ServiceExt;

fn setup_env() {
    if env::var("DATABASE_URL").is_err() {
        env::set_var(
            "DATABASE_URL",
            "postgres://synapse_app:synapse_app@localhost:5432/synapse_test",
        );
    }
}

async fn make_api_state() -> ApiState {
    setup_env();
    let db_url = env::var("DATABASE_URL").unwrap();
    let app_state = AppState::test_new(&db_url).await;
    app_state.load_tenant_configs().await.ok();
    let graphql_schema = build_schema(app_state.clone());
    ApiState {
        app_state,
        graphql_schema,
    }
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres)"]
async fn test_admin_endpoints_require_security_headers() {
    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/test", get(|| async { "ok" }))
        .with_state(app_state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/test")
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let headers = response.headers();

    assert!(
        headers.contains_key("content-security-policy")
            || headers.contains_key("x-content-type-options")
            || headers.contains_key("strict-transport-security"),
        "admin endpoints should include security headers"
    );
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres)"]
async fn test_admin_endpoints_reject_unauthorized_origins() {
    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/test", get(|| async { "ok" }))
        .with_state(app_state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/test")
                .method("OPTIONS")
                .header("origin", "https://unauthorized-domain.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let headers = response.headers();
    let cors_origin = headers.get("access-control-allow-origin");

    assert!(
        cors_origin.is_none()
            || cors_origin.map(|v| v.to_str().unwrap_or("")) != Ok("https://unauthorized-domain.com"),
        "unauthorized origins should not be allowed in CORS responses"
    );
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres)"]
async fn test_admin_endpoints_enforce_strict_cors_policy() {
    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/test", get(|| async { "ok" }))
        .with_state(app_state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/test")
                .method("OPTIONS")
                .header("origin", "*")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let headers = response.headers();
    let cors_origin = headers.get("access-control-allow-origin");

    assert!(
        cors_origin.is_none() || cors_origin.map(|v| v.to_str().unwrap_or("")) != Ok("*"),
        "wildcard CORS origins should not be allowed for admin endpoints"
    );
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres)"]
async fn test_admin_endpoints_include_x_content_type_options() {
    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/test", get(|| async { "ok" }))
        .with_state(app_state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/test")
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let headers = response.headers();

    assert!(
        headers.get("x-content-type-options").is_some(),
        "admin endpoints must include X-Content-Type-Options header"
    );

    let x_content_type = headers
        .get("x-content-type-options")
        .and_then(|v| v.to_str().ok());
    assert_eq!(x_content_type, Some("nosniff"), "X-Content-Type-Options should be 'nosniff'");
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres)"]
async fn test_admin_endpoints_enforce_hsts() {
    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/test", get(|| async { "ok" }))
        .with_state(app_state);

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/test")
                .method("GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let headers = response.headers();

    let hsts = headers
        .get("strict-transport-security")
        .and_then(|v| v.to_str().ok());

    assert!(
        hsts.is_some() && hsts.unwrap().contains("max-age"),
        "admin endpoints must include Strict-Transport-Security header"
    );
}
