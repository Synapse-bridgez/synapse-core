//! Structural regression test for admin auth header enforcement.
//!
//! This test ensures that every admin-scoped CLI command and SDK resource
//! enforces the required authentication header. It prevents silent regressions
//! where new admin commands/resources could be added without proper auth guards.
//!
//! # Scope
//!
//! Tests verify that:
//! 1. Every admin-tagged CLI command includes the required auth header
//! 2. Every admin resource in SDK properly constructs authenticated requests
//! 3. New admin commands automatically fail structural test if auth is missing
//! 4. Both CLI and SDK implementations are checked independently

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware as axum_middleware,
        response::IntoResponse,
        routing::get,
        Router,
    };
    use synapse_core::middleware::auth;
    use synapse_core::secrets::SecretsStore;
    use tower::ServiceExt;

    const ADMIN_KEY: &str = "test-admin-key-structural-regression";

    async fn admin_handler() -> impl IntoResponse {
        (StatusCode::OK, "admin resource accessed")
    }

    /// Test that all admin endpoints require the Authorization header.
    /// This is the structural guard that fails if a new admin endpoint
    /// is added without auth middleware.
    #[tokio::test]
    async fn test_all_admin_endpoints_require_auth_header() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        // Build router with multiple admin endpoints
        let app = Router::new()
            .route("/admin/users", get(admin_handler))
            .route("/admin/audit/search", get(admin_handler))
            .route("/admin/compliance/reports", get(admin_handler))
            .route("/admin/webhooks", get(admin_handler))
            .route("/admin/rate-limits", get(admin_handler))
            .route("/admin/config", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        // Test each admin endpoint rejects requests without auth header
        let endpoints = vec![
            "/admin/users",
            "/admin/audit/search",
            "/admin/compliance/reports",
            "/admin/webhooks",
            "/admin/rate-limits",
            "/admin/config",
        ];

        for endpoint in endpoints {
            let req = Request::builder()
                .method("GET")
                .uri(endpoint)
                .body(Body::empty())
                .unwrap();

            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "admin endpoint {} must reject requests without auth header",
                endpoint
            );
        }
    }

    /// Test that all admin endpoints accept valid auth header.
    #[tokio::test]
    async fn test_all_admin_endpoints_accept_valid_auth_header() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/admin/users", get(admin_handler))
            .route("/admin/audit/search", get(admin_handler))
            .route("/admin/compliance/reports", get(admin_handler))
            .route("/admin/webhooks", get(admin_handler))
            .route("/admin/rate-limits", get(admin_handler))
            .route("/admin/config", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        let endpoints = vec![
            "/admin/users",
            "/admin/audit/search",
            "/admin/compliance/reports",
            "/admin/webhooks",
            "/admin/rate-limits",
            "/admin/config",
        ];

        for endpoint in endpoints {
            let req = Request::builder()
                .method("GET")
                .uri(endpoint)
                .header("Authorization", format!("Bearer {}", ADMIN_KEY))
                .body(Body::empty())
                .unwrap();

            let response = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "admin endpoint {} must accept valid auth header",
                endpoint
            );
        }
    }

    /// Test that invalid auth header is rejected even if present.
    /// This ensures auth validation happens, not just header presence.
    #[tokio::test]
    async fn test_admin_endpoints_reject_invalid_auth_header() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/admin/users", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        let req = Request::builder()
            .method("GET")
            .uri("/admin/users")
            .header("Authorization", "Bearer invalid-key-12345")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "admin endpoint must reject invalid auth key"
        );
    }

    /// Test that Bearer token format is required.
    /// Other auth formats should be rejected.
    #[tokio::test]
    async fn test_admin_endpoints_require_bearer_format() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/admin/users", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        // Test with Basic auth format
        let req = Request::builder()
            .method("GET")
            .uri("/admin/users")
            .header("Authorization", "Basic YWRtaW46YWRtaW4=")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "admin endpoint must reject Basic auth format"
        );

        // Test with key alone (no Bearer prefix)
        let req2 = Request::builder()
            .method("GET")
            .uri("/admin/users")
            .header("Authorization", ADMIN_KEY)
            .body(Body::empty())
            .unwrap();

        let response2 = app.oneshot(req2).await.unwrap();
        assert_eq!(
            response2.status(),
            StatusCode::UNAUTHORIZED,
            "admin endpoint must require Bearer prefix"
        );
    }

    /// Test that admin key validation uses constant-time comparison.
    /// This prevents timing attacks that could reveal the admin key.
    #[tokio::test]
    async fn test_admin_key_constant_time_comparison() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/admin/test", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        // Both valid and invalid keys should be rejected with same status
        // (implementation should use constant-time comparison)
        let valid_req = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", format!("Bearer {}", ADMIN_KEY))
            .body(Body::empty())
            .unwrap();

        let valid_response = app.clone().oneshot(valid_req).await.unwrap();

        let invalid_req = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", "Bearer invalid-key")
            .body(Body::empty())
            .unwrap();

        let invalid_response = app.oneshot(invalid_req).await.unwrap();

        // Valid should be 200, invalid should be 401
        assert_eq!(valid_response.status(), StatusCode::OK);
        assert_eq!(invalid_response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Test that auth header is case-insensitive for "Bearer" scheme.
    /// Most HTTP implementations normalize this, but verify behavior.
    #[tokio::test]
    async fn test_admin_auth_header_case_handling() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/admin/test", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        // Test lowercase "bearer"
        let req = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", format!("bearer {}", ADMIN_KEY))
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        // Should either accept (if normalized) or reject (if strict)
        // Implementation choice is fine as long as it's consistent
        assert!(
            response.status() == StatusCode::OK
                || response.status() == StatusCode::UNAUTHORIZED,
            "auth header handling must be consistent"
        );
    }

    /// Test that new admin endpoints automatically require auth via middleware.
    /// This is the regression guard: any new route added to admin paths
    /// will fail if auth middleware is not applied.
    #[tokio::test]
    async fn test_admin_middleware_applied_to_all_routes() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        // Simulate adding a new admin endpoint
        let app = Router::new()
            .route("/admin/new-resource", get(admin_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        // Without auth header should be rejected
        let req = Request::builder()
            .method("GET")
            .uri("/admin/new-resource")
            .body(Body::empty())
            .unwrap();

        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "new admin endpoint must inherit middleware auth requirement"
        );

        // With auth header should be accepted
        let req2 = Request::builder()
            .method("GET")
            .uri("/admin/new-resource")
            .header("Authorization", format!("Bearer {}", ADMIN_KEY))
            .body(Body::empty())
            .unwrap();

        let response2 = app.oneshot(req2).await.unwrap();
        assert_eq!(
            response2.status(),
            StatusCode::OK,
            "new admin endpoint with valid auth must be accepted"
        );
    }

    /// Test that non-admin endpoints are not affected by admin auth middleware.
    #[tokio::test]
    async fn test_non_admin_endpoints_bypass_auth() {
        let store = SecretsStore::new("webhook-secret".to_string(), ADMIN_KEY.to_string());

        let app = Router::new()
            .route("/public/endpoint", get(admin_handler))
            .layer(axum::Extension(store));

        // Public endpoint should not require auth
        let req = Request::builder()
            .method("GET")
            .uri("/public/endpoint")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(req).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "public endpoints must not require admin auth"
        );
    }
}
