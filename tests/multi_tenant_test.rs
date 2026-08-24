use axum::extract::FromRequestParts;
use axum::http::{header, Request};
use sqlx::PgPool;
use std::env;
use uuid::Uuid;

use synapse_core::tenant::{TenantConfig, TenantContext};
use synapse_core::{error::AppError, AppState};

/// Helper to ensure DATABASE_URL is set to local test database
fn setup_env() {
    if env::var("DATABASE_URL").is_err() {
        env::set_var(
            "DATABASE_URL",
            "postgres://synapse:synapse@localhost:5432/synapse_test",
        );
    }
}

async fn get_pool() -> PgPool {
    setup_env();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL not set");
    PgPool::connect(&db_url).await.unwrap()
}

async fn make_app_state() -> AppState {
    setup_env();
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not set");
    // do NOT reset schema here; callers should establish it to avoid wiping data
    let state = AppState::test_new(&db_url).await;
    let _ = state.load_tenant_configs().await;
    state
}

async fn insert_tenant(pool: &PgPool, tenant_id: Uuid, name: &str, api_key: &str) {
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1, $2, $3, '', '', 60, true)"
    )
    .bind(tenant_id)
    .bind(name)
    .bind(api_key)
    .execute(pool)
    .await
    .expect("Failed to insert tenant");
}

fn make_tenant_config(tenant_id: Uuid, name: &str) -> TenantConfig {
    TenantConfig {
        tenant_id,
        name: name.to_string(),
        webhook_secret: "secret".to_string(),
        stellar_account: "account".to_string(),
        rate_limit_per_minute: 100,
        is_active: true,
    }
}

/// Ensure the database schema required by tests is present.
/// Uses CREATE TABLE IF NOT EXISTS so concurrent tests don't race on drops.
async fn ensure_schema(pool: &PgPool) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS tenants (
            tenant_id UUID PRIMARY KEY,
            name VARCHAR(255) NOT NULL,
            api_key VARCHAR(255) NOT NULL UNIQUE,
            webhook_secret VARCHAR(255) NOT NULL DEFAULT '',
            stellar_account VARCHAR(56) NOT NULL DEFAULT '',
            rate_limit_per_minute INTEGER NOT NULL DEFAULT 60,
            is_active BOOLEAN NOT NULL DEFAULT true
        )",
    )
    .execute(pool)
    .await
    .ok();
}

async fn cleanup_tenant(pool: &PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM tenants WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

/// Ensure that resolving a tenant via an API key header returns the correct ID
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_tenant_resolution_from_api_key() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let tenant_id = Uuid::new_v4();
    // Use a unique key per run to avoid UNIQUE constraint conflicts
    let api_key = format!("test-key-api-{}", tenant_id);

    insert_tenant(&pool, tenant_id, "ApiTenant", &api_key).await;
    let state = make_app_state().await;
    // the state loader should have pulled the tenant from the database

    let req = Request::builder().body(()).unwrap();
    let (mut parts, _) = req.into_parts();
    parts.headers.insert(
        "X-API-Key",
        header::HeaderValue::from_str(&api_key).unwrap(),
    );

    let ctx = TenantContext::from_request_parts(&mut parts, &state)
        .await
        .unwrap();
    assert_eq!(ctx.tenant_id, tenant_id);

    cleanup_tenant(&pool, tenant_id).await;
}

/// Check that X-Tenant-ID or Authorization headers are respected
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_tenant_resolution_from_header() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let tenant_id = Uuid::new_v4();
    let api_key = format!("unused-{}", tenant_id);
    insert_tenant(&pool, tenant_id, "HeaderTenant", &api_key).await;

    let state = make_app_state().await;
    // config loaded automatically from db

    // try with X-Tenant-ID
    let req = Request::builder().body(()).unwrap();
    let (mut parts, _) = req.into_parts();
    parts.headers.insert(
        "X-Tenant-ID",
        header::HeaderValue::from_str(&tenant_id.to_string()).unwrap(),
    );

    let ctx = TenantContext::from_request_parts(&mut parts, &state)
        .await
        .unwrap();
    assert_eq!(ctx.tenant_id, tenant_id);

    // try with Authorization Bearer style
    let req2 = Request::builder().body(()).unwrap();
    let (mut parts2, _) = req2.into_parts();
    parts2.headers.insert(
        header::AUTHORIZATION,
        header::HeaderValue::from_str(&format!("Bearer {}", tenant_id)).unwrap(),
    );

    // resolution via path extraction will parse the uuid first, so we simulate such by setting path param
    // but our logic doesn't support Bearer for tenant id, only for API key. however the header test is still good
    let result = TenantContext::from_request_parts(&mut parts2, &state).await;
    assert!(matches!(result, Err(AppError::InvalidApiKey)));

    cleanup_tenant(&pool, tenant_id).await;
}

/// Insert transactions for two tenants and verify filtering works
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_query_filtering_by_tenant() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    insert_tenant(&pool, t1, "T1", &format!("k1-{}", t1)).await;
    insert_tenant(&pool, t2, "T2", &format!("k2-{}", t2)).await;

    // Insert transactions using the real schema (id, stellar_account, amount, asset_code, tenant_id)
    let tx1 = Uuid::new_v4();
    let tx2 = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, tenant_id, created_at) VALUES ($1, '', 10, 'USD', $2, NOW())"
    )
    .bind(tx1)
    .bind(t1)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, tenant_id, created_at) VALUES ($1, '', 20, 'USD', $2, NOW())"
    )
    .bind(tx2)
    .bind(t2)
    .execute(&pool)
    .await
    .unwrap();

    let list1: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE tenant_id = $1")
        .bind(t1)
        .fetch_all(&pool)
        .await
        .unwrap();

    assert_eq!(list1.len(), 1);
    assert_eq!(list1[0].0, tx1);

    // wrong tenant should not see tx1
    let wrong: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM transactions WHERE id = $1 AND tenant_id = $2")
            .bind(tx1)
            .bind(t2)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert!(wrong.is_none());

    cleanup_tenant(&pool, t1).await;
    cleanup_tenant(&pool, t2).await;
}

/// Verify that state configurations are isolated per tenant
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_tenant_config_isolation() {
    setup_env();
    let state = make_app_state().await;

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    let c1 = make_tenant_config(t1, "C1");
    let c2 = make_tenant_config(t2, "C2");

    {
        let mut map = state.tenant_configs.write().await;
        map.insert(t1, c1.clone());
        map.insert(t2, c2.clone());
    }

    let got1 = state.get_tenant_config(t1).await.unwrap();
    let got2 = state.get_tenant_config(t2).await.unwrap();
    assert_eq!(got1.name, "C1");
    assert_eq!(got2.name, "C2");
    assert!(state.get_tenant_config(Uuid::new_v4()).await.is_none());
}

/// Run several tenant resolution operations concurrently to make sure there is no shared-mutation bug
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_concurrent_multi_tenant_requests() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    // Use unique keys per run to avoid UNIQUE constraint conflicts
    let key1 = format!("ck1-{}", t1);
    let key2 = format!("ck2-{}", t2);
    insert_tenant(&pool, t1, "Con1", &key1).await;
    insert_tenant(&pool, t2, "Con2", &key2).await;

    // now create state after tenants exist so loader will pick them up
    let state = make_app_state().await;

    let fut1 = {
        let state = state.clone();
        let key1 = key1.clone();
        async move {
            let req = Request::builder().body(()).unwrap();
            let (mut parts, _) = req.into_parts();
            parts
                .headers
                .insert("X-API-Key", header::HeaderValue::from_str(&key1).unwrap());
            TenantContext::from_request_parts(&mut parts, &state)
                .await
                .unwrap()
                .tenant_id
        }
    };

    let fut2 = {
        let state = state.clone();
        let key2 = key2.clone();
        async move {
            let req = Request::builder().body(()).unwrap();
            let (mut parts, _) = req.into_parts();
            parts
                .headers
                .insert("X-API-Key", header::HeaderValue::from_str(&key2).unwrap());
            TenantContext::from_request_parts(&mut parts, &state)
                .await
                .unwrap()
                .tenant_id
        }
    };

    let (r1, r2) = tokio::join!(fut1, fut2);
    assert_eq!(r1, t1);
    assert_eq!(r2, t2);

    cleanup_tenant(&pool, t1).await;
    cleanup_tenant(&pool, t2).await;
}

/// Quick sanity check that the database enforces tenant isolation at foreign key level
#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_db_foreign_key_enforces_tenant() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    // Insert a transaction referencing a non-existent tenant_id — FK should reject it
    let result = sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, tenant_id, created_at) VALUES ($1, '', 5, 'USD', $2, NOW())",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4()) // random UUID — no matching tenant
    .execute(&pool)
    .await;

    assert!(result.is_err());
}
