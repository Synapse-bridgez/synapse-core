/// Integration test for the Part B fix: PUT /admin/quotas/:tenant_id used to
/// write a Redis config (`quota:config:{key}`) the live rate limiter never
/// read (see src/handlers/admin/quota.rs::set_tenant_quota and
/// src/middleware/quota.rs::rate_limit_middleware). An admin got a 200 and
/// reasonably believed the enforced limit changed; it didn't.
///
/// This test proves the fix end-to-end: call the admin endpoint, then make
/// real requests through `rate_limit_middleware` and confirm the *new*
/// limit — not the tenant's original one — is what actually gets enforced.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use sqlx::PgPool;
use std::env;
use synapse_core::{graphql::schema::build_schema, handlers::admin::quota, ApiState, AppState};
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
        "INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1, $2, $3, '', '', $4, true)",
    )
    .bind(tenant_id)
    .bind(format!("QuotaEnforcementTenant-{tenant_id}"))
    .bind(format!("key-{tenant_id}"))
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
async fn admin_quota_update_is_actually_enforced_by_the_live_rate_limiter() {
    let pool = get_pool().await;
    let tenant_id = Uuid::new_v4();
    // Start generous so the test can tell "still the old limit" apart from
    // "the new limit" instead of both looking like 429s.
    insert_tenant(&pool, tenant_id, 1000).await;

    let api_state = make_api_state().await;

    // 1. Call the admin endpoint to override the limit down to 2/min.
    let response = quota::set_tenant_quota(
        axum::extract::State(api_state.clone()),
        axum::extract::Path(tenant_id),
        axum::Json(quota::SetQuotaRequest {
            custom_limit: Some(2),
        }),
    )
    .await
    .expect("set_tenant_quota should succeed")
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);

    // 2. Build a real router with the live rate_limit_middleware layered —
    //    the same middleware every core_routes/callback/webhook route uses —
    //    and drive real requests through it.
    let app_state = api_state.app_state.clone();
    let router: Router = Router::new()
        .route("/probe", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            app_state.clone(),
            synapse_core::middleware::quota::rate_limit_middleware,
        ))
        .with_state(app_state);

    // NOTE: rate_limit_middleware matches the `X-API-Key` header against
    // `tenant_id.to_string()`, not the tenant's actual `api_key` column —
    // that's an existing quirk of this middleware's key-matching logic,
    // unrelated to and unchanged by this fix. Sending the tenant_id as the
    // header value is what makes the *tenant-specific* limit apply instead
    // of the anonymous default.
    let mut allowed = 0;
    let mut rejected = 0;
    for _ in 0..5 {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/probe")
                    .header("X-API-Key", tenant_id.to_string())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if response.status() == StatusCode::OK {
            allowed += 1;
        } else if response.status() == StatusCode::TOO_MANY_REQUESTS {
            rejected += 1;
        }
    }

    assert_eq!(
        allowed, 2,
        "exactly 2 requests should succeed — the limit set via the admin endpoint, \
         not the tenant's original 1000/min limit"
    );
    assert_eq!(rejected, 3, "the remaining requests should be rate-limited");

    cleanup_tenant(&pool, tenant_id).await;
}
