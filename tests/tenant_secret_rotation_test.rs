//! Tenant Secret Rotation & Grace Period Test Suite.

use chrono::{Duration, Utc};
use synapse_core::db::audit::ENTITY_TENANT;
use synapse_core::db::queries::{get_audit_logs, revoke_expired_tenant_secrets, rotate_tenant_secret};
use synapse_core::handlers::admin::tenant_rotation::verify_admin_auth;
use synapse_core::tenant::TenantConfig;
use axum::http::HeaderMap;
use uuid::Uuid;


fn get_database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok()
}

#[test]
fn test_tenant_secret_dual_validation_grace_period() {
    let now = Utc::now();
    let config = TenantConfig {
        tenant_id: Uuid::new_v4(),
        name: "Test Tenant".to_string(),
        webhook_secret: "new_active_secret_123".to_string(),
        previous_webhook_secret: Some("old_secret_456".to_string()),
        previous_secret_expires_at: Some(now + Duration::hours(24)), // Active grace period
        secret_updated_at: Some(now),
        stellar_account: "GBRPYHIL2CI3FNQ4BXLFMNDLFPPPU2HY5BTH4TTJ3HQ6N4D6L546M7B5".to_string(),
        rate_limit_per_minute: 60,
        is_active: true,
    };

    // 1. Current secret MUST validate
    assert!(
        config.validate_webhook_secret("new_active_secret_123"),
        "Current secret must validate during grace period"
    );

    // 2. Previous secret MUST validate during grace period
    assert!(
        config.validate_webhook_secret("old_secret_456"),
        "Previous secret must validate while within active grace period"
    );

    // 3. Invalid secret MUST fail validation
    assert!(
        !config.validate_webhook_secret("invalid_secret_789"),
        "Invalid secret must fail validation"
    );
}

#[test]
fn test_tenant_secret_post_expiry_revocation() {
    let now = Utc::now();
    let config = TenantConfig {
        tenant_id: Uuid::new_v4(),
        name: "Test Tenant Expired".to_string(),
        webhook_secret: "new_active_secret_123".to_string(),
        previous_webhook_secret: Some("old_secret_456".to_string()),
        previous_secret_expires_at: Some(now - Duration::seconds(10)), // Expired grace period
        secret_updated_at: Some(now - Duration::hours(48)),
        stellar_account: "GBRPYHIL2CI3FNQ4BXLFMNDLFPPPU2HY5BTH4TTJ3HQ6N4D6L546M7B5".to_string(),
        rate_limit_per_minute: 60,
        is_active: true,
    };

    // 1. Current secret MUST validate
    assert!(
        config.validate_webhook_secret("new_active_secret_123"),
        "Current secret must validate"
    );

    // 2. Expired previous secret MUST be revoked (fail validation)
    assert!(
        !config.validate_webhook_secret("old_secret_456"),
        "Expired previous secret MUST be revoked after grace period expires"
    );
}

#[test]
fn test_admin_auth_verification_rules() {
    let mut headers = HeaderMap::new();

    // Missing headers -> Forbidden
    assert!(verify_admin_auth(&headers).is_err());

    // User role -> Forbidden
    headers.insert("X-Admin-Role", "user".parse().unwrap());
    assert!(verify_admin_auth(&headers).is_err());

    // Admin role -> Authorized
    headers.insert("X-Admin-Role", "admin".parse().unwrap());
    assert!(verify_admin_auth(&headers).is_ok());

    // Superadmin role -> Authorized
    headers.insert("X-Admin-Role", "superadmin".parse().unwrap());
    assert!(verify_admin_auth(&headers).is_ok());
}

#[tokio::test]
async fn test_tenant_secret_rotation_db_and_audit_trail() {
    let database_url = match get_database_url() {
        Some(url) => url,
        None => {
            println!("Skipping DB tenant secret rotation test: DATABASE_URL not set");
            return;
        }
    };

    let pool = match sqlx::PgPool::connect(&database_url).await {
        Ok(p) => p,
        Err(e) => {
            println!("Skipping DB test (connect failed): {:?}", e);
            return;
        }
    };

    let tenant_id = Uuid::new_v4();

    // Create test tenant
    let insert_res = sqlx::query(
        r#"
        INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, is_active)
        VALUES ($1, $2, $3, $4, $5, true)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .bind("Rotation Test Tenant")
    .bind(format!("key_{}", tenant_id))
    .bind("initial_secret_123")
    .bind("GBRPYHIL2CI3FNQ4BXLFMNDLFPPPU2HY5BTH4TTJ3HQ6N4D6L546M7B5")
    .execute(&pool)
    .await;

    if let Err(e) = insert_res {
        println!("Skipping DB execution (tenants table missing/not ready): {:?}", e);
        return;
    }

    // Perform secret rotation with 3600s (1h) grace period
    let rotation = rotate_tenant_secret(&pool, tenant_id, Some("rotated_secret_789".to_string()), 3600, "operator_alice")
        .await
        .expect("Failed to rotate tenant secret");

    assert_eq!(rotation.tenant_id, tenant_id);
    assert_eq!(rotation.new_secret, "rotated_secret_789");

    // Fetch tenant config and verify dual validation
    let tenant_cfg = synapse_core::db::queries::get_tenant_config_by_id(&pool, tenant_id)
        .await
        .expect("Failed to fetch tenant config");

    assert_eq!(tenant_cfg.webhook_secret, "rotated_secret_789");
    assert_eq!(tenant_cfg.previous_webhook_secret, Some("initial_secret_123".to_string()));
    assert!(tenant_cfg.validate_webhook_secret("rotated_secret_789"));
    assert!(tenant_cfg.validate_webhook_secret("initial_secret_123"));

    // Verify audit log entry for issuance
    let audit_logs = get_audit_logs(&pool, tenant_id, 10, 0)
        .await
        .expect("Failed to fetch audit logs");

    assert!(!audit_logs.is_empty(), "Audit logs must record secret rotation issuance");
    let (_, entity_id, entity_type, action, _, _, actor, _) = &audit_logs[0];
    assert_eq!(*entity_id, tenant_id);
    assert_eq!(entity_type, ENTITY_TENANT);
    assert_eq!(action, "secret_rotation_issued");
    assert_eq!(actor, "operator_alice");

    // Manually trigger expired secret revocation
    sqlx::query("UPDATE tenants SET previous_secret_expires_at = NOW() - INTERVAL '1 minute' WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&pool)
        .await
        .unwrap();

    let revoked_count = revoke_expired_tenant_secrets(&pool)
        .await
        .expect("Failed to revoke expired secrets");

    assert!(revoked_count >= 1);

    let updated_cfg = synapse_core::db::queries::get_tenant_config_by_id(&pool, tenant_id).await.unwrap();
    assert!(!updated_cfg.validate_webhook_secret("initial_secret_123"), "Expired initial secret MUST be revoked");
}
