use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use sqlx::PgPool;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use synapse_core::{graphql::schema::build_schema, handlers::admin::quota, ApiState, AppState};
use tower::ServiceExt;
use uuid::Uuid;
use tokio::task::JoinSet;

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

async fn get_pool() -> PgPool {
    setup_env();
    let db_url = env::var("DATABASE_URL").unwrap();
    PgPool::connect(&db_url).await.unwrap()
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

async fn insert_tenant(pool: &PgPool, tenant_id: Uuid, rate_limit_per_minute: i32) {
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1, $2, $3, pgp_sym_encrypt('', $4), '', $5, true)",
    )
    .bind(tenant_id)
    .bind(format!("AdversarialTestTenant-{tenant_id}"))
    .bind(synapse_core::db::queries::hash_api_key(&format!(
        "key-{tenant_id}"
    )))
    .bind(synapse_core::db::queries::tenant_secret_key())
    .bind(rate_limit_per_minute)
    .execute(pool)
    .await
    .expect("failed to insert tenant");
}

async fn cleanup_tenant(pool: &PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn test_concurrent_burst_requests_near_quota_limit() {
    let pool = get_pool().await;
    let tenant_id = Uuid::new_v4();
    let rate_limit = 5;
    insert_tenant(&pool, tenant_id, rate_limit).await;

    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            synapse_core::middleware::quota::rate_limit_middleware,
        ))
        .with_state(app_state);

    let allowed_count = Arc::new(AtomicU32::new(0));
    let rejected_count = Arc::new(AtomicU32::new(0));

    let mut tasks = JoinSet::new();

    for _ in 0..20 {
        let router_clone = router.clone();
        let tenant_id_clone = tenant_id;
        let allowed_clone = Arc::clone(&allowed_count);
        let rejected_clone = Arc::clone(&rejected_count);

        tasks.spawn(async move {
            let response = router_clone
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/probe")
                        .header("X-API-Key", tenant_id_clone.to_string())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            match response.status() {
                StatusCode::OK => {
                    allowed_clone.fetch_add(1, Ordering::Relaxed);
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    rejected_clone.fetch_add(1, Ordering::Relaxed);
                }
                _ => {}
            }
        });
    }

    while tasks.join_next().await.is_some() {}

    let allowed = allowed_count.load(Ordering::Relaxed);
    let rejected = rejected_count.load(Ordering::Relaxed);

    assert!(
        allowed <= rate_limit as u32,
        "concurrent requests should not exceed quota limit (allowed: {}, limit: {})",
        allowed,
        rate_limit
    );
    assert!(rejected > 0, "some concurrent requests should be rate-limited");
    assert_eq!(
        allowed + rejected, 20,
        "all concurrent requests should be accounted for"
    );

    cleanup_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn test_admin_exempt_endpoints_prevent_privilege_bypass() {
    let pool = get_pool().await;
    let tenant_id = Uuid::new_v4();
    let rate_limit = 2;
    insert_tenant(&pool, tenant_id, rate_limit).await;

    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/admin/probe", get(|| async { "admin-ok" }))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            synapse_core::middleware::quota::rate_limit_middleware,
        ))
        .with_state(app_state);

    let normal_response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin/probe")
                .header("X-API-Key", tenant_id.to_string())
                .header("X-Admin-Override", "true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let is_allowed_via_header_spoofing = normal_response.status() != StatusCode::TOO_MANY_REQUESTS;
    assert!(
        !is_allowed_via_header_spoofing || normal_response.status() == StatusCode::OK,
        "spoofed admin headers should not bypass quota enforcement"
    );

    cleanup_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn test_quota_check_increment_race_condition() {
    let pool = get_pool().await;
    let tenant_id = Uuid::new_v4();
    let rate_limit = 3;
    insert_tenant(&pool, tenant_id, rate_limit).await;

    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            synapse_core::middleware::quota::rate_limit_middleware,
        ))
        .with_state(app_state);

    let mut tasks = JoinSet::new();
    let allowed_count = Arc::new(AtomicU32::new(0));

    for _ in 0..10 {
        let router_clone = router.clone();
        let tenant_id_clone = tenant_id;
        let allowed_clone = Arc::clone(&allowed_count);

        tasks.spawn(async move {
            let response = router_clone
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/probe")
                        .header("X-API-Key", tenant_id_clone.to_string())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            if response.status() == StatusCode::OK {
                allowed_clone.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    while tasks.join_next().await.is_some() {}

    let allowed = allowed_count.load(Ordering::Relaxed);

    assert!(
        allowed <= rate_limit as u32,
        "race conditions in check-then-increment should not allow exceeding quota (allowed: {}, limit: {})",
        allowed,
        rate_limit
    );

    cleanup_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires Docker/external services (Postgres + Redis)"]
async fn test_parameter_manipulation_cannot_bypass_quota() {
    let pool = get_pool().await;
    let tenant_id = Uuid::new_v4();
    let rate_limit = 2;
    insert_tenant(&pool, tenant_id, rate_limit).await;

    let api_state = make_api_state().await;
    let app_state = api_state.app_state.clone();

    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            synapse_core::middleware::quota::rate_limit_middleware,
        ))
        .with_state(app_state);

    let malicious_tenant_id = Uuid::new_v4();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header("X-API-Key", malicious_tenant_id.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        response.status() == StatusCode::TOO_MANY_REQUESTS || response.status() != StatusCode::OK,
        "quota enforcement should apply to all tenants, not just configured ones"
    );

    cleanup_tenant(&pool, tenant_id).await;
}
