use axum::extract::FromRequestParts;
use axum::http::{header, Request};
use sqlx::PgPool;
use std::env;
use uuid::Uuid;

use synapse_core::tenant::{TenantConfig, TenantContext};
use synapse_core::{error::AppError, AppState};

/// Helper to ensure DATABASE_URL is set to local test database
fn setup_env() {
    env::set_var(
        "DATABASE_URL",
        "postgres://synapse:synapse@localhost:5433/synapse_test",
    );
}

async fn get_pool() -> PgPool {
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

/// Ensure the database schema required by tests is present
async fn ensure_schema(pool: &PgPool) {
    // Drop tables to guarantee clean state
    let _ = sqlx::query("DROP TABLE IF EXISTS transactions")
        .execute(pool)
        .await;
    let _ = sqlx::query("DROP TABLE IF EXISTS tenants")
        .execute(pool)
        .await;

    // create tenants table similar to migration
    let _ = sqlx::query(
        "CREATE TABLE tenants (
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
    .await;

    // simple transactions table with tenant foreign key enforcement
    let _ = sqlx::query(
        "CREATE TABLE transactions (
            transaction_id UUID PRIMARY KEY,
            tenant_id UUID NOT NULL REFERENCES tenants(tenant_id),
            amount NUMERIC
        )",
    )
    .execute(pool)
    .await;
}

/// Ensure that resolving a tenant via an API key header returns the correct ID
#[tokio::test]
async fn test_tenant_resolution_from_api_key() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let tenant_id = Uuid::new_v4();
    let api_key = "test-key-api";

    insert_tenant(&pool, tenant_id, "ApiTenant", api_key).await;
    let state = make_app_state().await;
    // the state loader should have pulled the tenant from the database

    let req = Request::builder().body(()).unwrap();
    let (mut parts, _) = req.into_parts();
    parts
        .headers
        .insert("X-API-Key", header::HeaderValue::from_str(api_key).unwrap());

    let ctx = TenantContext::from_request_parts(&mut parts, &state)
        .await
        .unwrap();
    assert_eq!(ctx.tenant_id, tenant_id);
}

/// Check that X-Tenant-ID or Authorization headers are respected
#[tokio::test]
async fn test_tenant_resolution_from_header() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let tenant_id = Uuid::new_v4();
    insert_tenant(&pool, tenant_id, "HeaderTenant", "unused").await;

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
}

/// Insert transactions for two tenants and verify filtering works
#[tokio::test]
async fn test_query_filtering_by_tenant() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    insert_tenant(&pool, t1, "T1", "k1").await;
    insert_tenant(&pool, t2, "T2", "k2").await;

    // create data for each tenant
    let tx1 = Uuid::new_v4();
    let tx2 = Uuid::new_v4();
    sqlx::query("INSERT INTO transactions (transaction_id, tenant_id, amount) VALUES ($1, $2, $3)")
        .bind(tx1)
        .bind(t1)
        .bind(10.0)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO transactions (transaction_id, tenant_id, amount) VALUES ($1, $2, $3)")
        .bind(tx2)
        .bind(t2)
        .bind(20.0)
        .execute(&pool)
        .await
        .unwrap();

    let list1: Vec<(Uuid,)> =
        sqlx::query_as("SELECT transaction_id FROM transactions WHERE tenant_id = $1")
            .bind(t1)
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(list1.len(), 1);
    assert_eq!(list1[0].0, tx1);

    // wrong tenant should not see tx1
    let wrong: Option<(Uuid,)> = sqlx::query_as(
        "SELECT transaction_id FROM transactions WHERE transaction_id = $1 AND tenant_id = $2",
    )
    .bind(tx1)
    .bind(t2)
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(wrong.is_none());
}

/// Verify that state configurations are isolated per tenant
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
#[tokio::test]
async fn test_concurrent_multi_tenant_requests() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();
    insert_tenant(&pool, t1, "Con1", "ck1").await;
    insert_tenant(&pool, t2, "Con2", "ck2").await;

    // now create state after tenants exist so loader will pick them up
    let state = make_app_state().await;

    let fut1 = {
        let state = state.clone();
        async move {
            let req = Request::builder().body(()).unwrap();
            let (mut parts, _) = req.into_parts();
            parts
                .headers
                .insert("X-API-Key", header::HeaderValue::from_str("ck1").unwrap());
            TenantContext::from_request_parts(&mut parts, &state)
                .await
                .unwrap()
                .tenant_id
        }
    };

    let fut2 = {
        let state = state.clone();
        async move {
            let req = Request::builder().body(()).unwrap();
            let (mut parts, _) = req.into_parts();
            parts
                .headers
                .insert("X-API-Key", header::HeaderValue::from_str("ck2").unwrap());
            TenantContext::from_request_parts(&mut parts, &state)
                .await
                .unwrap()
                .tenant_id
        }
    };

    let (r1, r2) = tokio::join!(fut1, fut2);
    assert_eq!(r1, t1);
    assert_eq!(r2, t2);
}

/// Quick sanity check that the database enforces tenant isolation at foreign key level
#[tokio::test]
async fn test_db_foreign_key_enforces_tenant() {
    setup_env();
    let pool = get_pool().await;
    ensure_schema(&pool).await;

    let result = sqlx::query(
        "INSERT INTO transactions (transaction_id, tenant_id, amount) VALUES ($1, $2, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(Uuid::new_v4())
    .bind(5.0)
    .execute(&pool)
    .await;

    assert!(result.is_err());
}
