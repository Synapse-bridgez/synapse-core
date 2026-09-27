use axum::extract::FromRequestParts;
use axum::http::{header, Request, StatusCode};
use sqlx::{PgPool, Row};
use std::env;
use tower::ServiceExt;
use uuid::Uuid;

use synapse_core::db::queries::{
    hash_api_key, lookup_api_key, revoke_expired_tenant_secrets, revoke_tenant_previous_secret,
    rotate_tenant_api_key, tenant_secret_key,
};
use synapse_core::tenant::TenantContext;
use synapse_core::{create_app, error::AppError, AppState};

fn setup_env() {
    if env::var("DATABASE_URL").is_err() {
        env::set_var(
            "DATABASE_URL",
            "postgres://synapse_app:synapse_app@localhost:5432/synapse_test",
        );
    }
    if env::var("ADMIN_API_KEY").is_err() {
        env::set_var("ADMIN_API_KEY", "test-admin-key");
    }
}

async fn get_pool() -> Option<PgPool> {
    setup_env();
    let db_url = env::var("DATABASE_URL").ok()?;
    sqlx::postgres::PgPoolOptions::new()
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SELECT set_config('app.is_admin', 'true', false)")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&db_url)
        .await
        .ok()
}

async fn insert_test_tenant(pool: &PgPool, tenant_id: Uuid, name: &str, api_key: &str) {
    let hash = hash_api_key(api_key);
    let secret_key = tenant_secret_key();
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active)
         VALUES ($1, $2, $3, pgp_sym_encrypt('whsec_test', $4), 'GA1234567890', 60, true)
         ON CONFLICT (tenant_id) DO UPDATE SET api_key_hash = $3, previous_api_key_hash = NULL, grace_period_expires_at = NULL",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(hash)
    .bind(secret_key)
    .execute(pool)
    .await
    .expect("Failed to insert test tenant");
}

async fn cleanup_test_tenant(pool: &PgPool, tenant_id: Uuid) {
    let _ = sqlx::query("DELETE FROM audit_logs WHERE entity_id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM tenants WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "Requires a live database"]
async fn test_tenant_secret_rotation_dual_validation_and_revocation() {
    let pool = match get_pool().await {
        Some(p) => p,
        None => return,
    };

    let tenant_id = Uuid::new_v4();
    let initial_key = "sk_live_initial_key_12345";
    let rotated_key = "sk_live_rotated_key_67890";

    insert_test_tenant(&pool, tenant_id, "Rotation Test Tenant", initial_key).await;

    // 1. Verify initial key works
    let resolved = lookup_api_key(&pool, initial_key)
        .await
        .expect("lookup failed");
    assert_eq!(resolved, Some(tenant_id));

    // 2. Rotate key with 3600s grace period
    let rotation = rotate_tenant_api_key(
        &pool,
        tenant_id,
        Some(rotated_key.to_string()),
        3600,
        "operator_alice",
    )
    .await
    .expect("rotation query failed");

    assert_eq!(rotation.tenant_id, tenant_id);
    assert_eq!(rotation.new_api_key, rotated_key);
    assert!(rotation.previous_api_key_hash.is_some());
    assert!(rotation.grace_period_expires_at.is_some());

    // 3. Grace-period dual validation: both old and new keys must succeed
    let old_lookup = lookup_api_key(&pool, initial_key)
        .await
        .expect("old key lookup failed");
    assert_eq!(
        old_lookup,
        Some(tenant_id),
        "Old key must validate during grace period"
    );

    let new_lookup = lookup_api_key(&pool, rotated_key)
        .await
        .expect("new key lookup failed");
    assert_eq!(
        new_lookup,
        Some(tenant_id),
        "New key must validate during grace period"
    );

    // 4. Audit trail completeness for secret issuance
    let issuance_audit = sqlx::query(
        "SELECT action, actor, old_val, new_val FROM audit_logs WHERE entity_id = $1 AND action = 'secret_issued' ORDER BY timestamp DESC LIMIT 1"
    )
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .expect("Secret issuance audit log missing");

    let actor: String = issuance_audit.get("actor");
    assert_eq!(actor, "operator_alice");

    // 5. Explicit revocation of previous secret
    let revoked = revoke_tenant_previous_secret(&pool, tenant_id, "operator_bob")
        .await
        .expect("revocation failed");
    assert!(revoked, "Expected previous secret to be revoked");

    // 6. Post-revocation: old key rejected, new key accepted
    let old_lookup_post_revoke = lookup_api_key(&pool, initial_key)
        .await
        .expect("old key lookup failed");
    assert_eq!(
        old_lookup_post_revoke, None,
        "Old key must be rejected after revocation"
    );

    let new_lookup_post_revoke = lookup_api_key(&pool, rotated_key)
        .await
        .expect("new key lookup failed");
    assert_eq!(
        new_lookup_post_revoke,
        Some(tenant_id),
        "New key must still validate after revocation of old key"
    );

    // 7. Audit trail completeness for secret revocation
    let revocation_audit = sqlx::query(
        "SELECT action, actor FROM audit_logs WHERE entity_id = $1 AND action = 'secret_revoked' ORDER BY timestamp DESC LIMIT 1"
    )
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .expect("Secret revocation audit log missing");

    let rev_actor: String = revocation_audit.get("actor");
    assert_eq!(rev_actor, "operator_bob");

    cleanup_test_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires a live database"]
async fn test_tenant_secret_rotation_post_expiry_rejection() {
    let pool = match get_pool().await {
        Some(p) => p,
        None => return,
    };

    let tenant_id = Uuid::new_v4();
    let key1 = "sk_test_key_1";
    let key2 = "sk_test_key_2";

    insert_test_tenant(&pool, tenant_id, "Expiry Test Tenant", key1).await;

    // Rotate with 10s grace
    rotate_tenant_api_key(&pool, tenant_id, Some(key2.to_string()), 10, "admin")
        .await
        .expect("rotation failed");

    // Manually simulate expiration in DB (grace_period_expires_at in the past)
    sqlx::query(
        "UPDATE tenants SET grace_period_expires_at = NOW() - INTERVAL '10 seconds' WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .execute(&pool)
    .await
    .expect("Failed to backdate expiry");

    // Even before background cleanup runs, query logic must reject expired old key
    let old_res = lookup_api_key(&pool, key1).await.unwrap();
    assert_eq!(
        old_res, None,
        "Expired grace-period key must not be accepted"
    );

    let new_res = lookup_api_key(&pool, key2).await.unwrap();
    assert_eq!(
        new_res,
        Some(tenant_id),
        "Current key must remain accepted"
    );

    // Run sweep revocation
    let cleaned = revoke_expired_tenant_secrets(&pool, "cron_sweep")
        .await
        .expect("sweep failed");
    assert!(cleaned >= 1);

    // Verify audit log recorded for expiry cleanup
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1 AND action = 'secret_revoked'",
    )
    .bind(tenant_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(
        audit_count >= 1,
        "Expiry revocation must be recorded in audit log"
    );

    cleanup_test_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires a live database"]
async fn test_tenant_context_extractor_with_rotated_keys() {
    let pool = match get_pool().await {
        Some(p) => p,
        None => return,
    };

    setup_env();
    let db_url = env::var("DATABASE_URL").unwrap();
    let state = AppState::test_new(&db_url).await;

    let tenant_id = Uuid::new_v4();
    let key_old = "sk_extractor_old_key";
    let key_new = "sk_extractor_new_key";

    insert_test_tenant(&pool, tenant_id, "Extractor Tenant", key_old).await;
    state.load_tenant_configs().await.unwrap();

    // Rotate key
    rotate_tenant_api_key(&pool, tenant_id, Some(key_new.to_string()), 3600, "admin")
        .await
        .unwrap();

    // Request with old key
    let mut req_old = Request::builder()
        .uri("/transactions")
        .header("X-API-Key", key_old)
        .body(())
        .unwrap();
    let (mut parts_old, _) = req_old.into_parts();
    let ctx_old = TenantContext::from_request_parts(&mut parts_old, &state).await;
    assert!(
        ctx_old.is_ok(),
        "TenantContext should succeed with old key during grace period"
    );
    assert_eq!(ctx_old.unwrap().tenant_id, tenant_id);

    // Request with new key
    let mut req_new = Request::builder()
        .uri("/transactions")
        .header("X-API-Key", key_new)
        .body(())
        .unwrap();
    let (mut parts_new, _) = req_new.into_parts();
    let ctx_new = TenantContext::from_request_parts(&mut parts_new, &state).await;
    assert!(
        ctx_new.is_ok(),
        "TenantContext should succeed with new key"
    );
    assert_eq!(ctx_new.unwrap().tenant_id, tenant_id);

    cleanup_test_tenant(&pool, tenant_id).await;
}

#[tokio::test]
#[ignore = "Requires a live database"]
async fn test_admin_api_rotate_and_revoke_endpoints() {
    let pool = match get_pool().await {
        Some(p) => p,
        None => return,
    };

    setup_env();
    let db_url = env::var("DATABASE_URL").unwrap();
    let state = AppState::test_new(&db_url).await;
    let app = create_app(state);

    let tenant_id = Uuid::new_v4();
    let initial_key = "sk_admin_test_init";
    insert_test_tenant(&pool, tenant_id, "Admin API Tenant", initial_key).await;

    // 1. Unauthorized call without Bearer token
    let unauth_req = Request::builder()
        .method("POST")
        .uri(format!("/admin/tenants/{tenant_id}/rotate-secret"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "grace_seconds": 1800 }).to_string(),
        ))
        .unwrap();

    let unauth_resp = app.clone().oneshot(unauth_req).await.unwrap();
    assert_eq!(unauth_resp.status(), StatusCode::UNAUTHORIZED);

    // 2. Authorized rotation call
    let auth_req = Request::builder()
        .method("POST")
        .uri(format!("/admin/tenants/{tenant_id}/rotate-secret"))
        .header("Authorization", "Bearer test-admin-key")
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(
            serde_json::json!({ "grace_seconds": 1800 }).to_string(),
        ))
        .unwrap();

    let auth_resp = app.clone().oneshot(auth_req).await.unwrap();
    assert_eq!(auth_resp.status(), StatusCode::OK);

    let body_bytes = hyper::body::to_bytes(auth_resp.into_body()).await.unwrap();
    let body_json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();

    let new_key = body_json["api_key"].as_str().unwrap();
    assert!(!new_key.is_empty());
    assert_eq!(body_json["grace_seconds"], 1800);

    // Both keys validate
    assert_eq!(lookup_api_key(&pool, initial_key).await.unwrap(), Some(tenant_id));
    assert_eq!(lookup_api_key(&pool, new_key).await.unwrap(), Some(tenant_id));

    // 3. Authorized revoke call
    let revoke_req = Request::builder()
        .method("POST")
        .uri(format!("/admin/tenants/{tenant_id}/revoke-secret"))
        .header("Authorization", "Bearer test-admin-key")
        .body(axum::body::Body::empty())
        .unwrap();

    let revoke_resp = app.clone().oneshot(revoke_req).await.unwrap();
    assert_eq!(revoke_resp.status(), StatusCode::OK);

    // Initial key rejected, new key active
    assert_eq!(lookup_api_key(&pool, initial_key).await.unwrap(), None);
    assert_eq!(lookup_api_key(&pool, new_key).await.unwrap(), Some(tenant_id));

    cleanup_test_tenant(&pool, tenant_id).await;
}
