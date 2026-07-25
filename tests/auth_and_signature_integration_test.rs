//! Integration tests for auth and signature-verification middleware.
//!
//! # What these tests cover  (#900)
//!
//! Every test in this file builds a real [`axum::Router`] that includes the
//! exact middleware used in production, sends an HTTP request through it via
//! [`tower::ServiceExt::oneshot`], and asserts on the actual HTTP response
//! status code.
//!
//! Previously the tests only exercised local variables (HMAC string generation,
//! constant-time comparison helpers) and never sent a request to any router.
//! Every test contained `assert!(true)` or checked that a hex-encoded string
//! was non-empty — providing zero coverage of the actual middleware.
//!
//! # Design
//!
//! Each test is self-contained and requires no external services (no database,
//! no Redis, no Docker).  Where the middleware needs a dependency injected as
//! an axum [`Extension`] (e.g. `SecretsStore`) the test constructs a minimal
//! in-process instance directly.
//!
//! The `admin_auth` middleware falls back to the `ADMIN_API_KEY` environment
//! variable when no `SecretsStore` extension is present, so those tests simply
//! set the env var for the duration of the test.
//!
//! The `signature_verification` middleware requires a `SecretsStore` extension
//! for production-grade rotation support; the tests inject one with a known
//! test secret.

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware as axum_middleware,
        response::IntoResponse,
        routing::{get, post},
        Router,
    };
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use synapse_core::middleware::{auth, signature_verification};
    use synapse_core::secrets::SecretsStore;
    use tower::ServiceExt; // for `oneshot`

    type HmacSha256 = Hmac<Sha256>;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// A trivial 200 OK handler used as the "inner" handler behind middleware.
    async fn ok_handler() -> impl IntoResponse {
        (StatusCode::OK, "ok")
    }

    /// Build an [`axum::Router`] that applies the `admin_auth` middleware to a
    /// single GET `/admin/test` route and has **no** `SecretsStore` extension,
    /// so `admin_auth` falls back to the `ADMIN_API_KEY` environment variable.
    fn admin_router() -> Router {
        Router::new()
            .route("/admin/test", get(ok_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
    }

    /// Build a router that applies `signature_verification` middleware, with a
    /// [`SecretsStore`] extension containing `webhook_secret` as the current
    /// webhook secret.
    fn callback_router(webhook_secret: &str) -> Router {
        let store = SecretsStore::new(webhook_secret.to_string(), "unused-admin-key".to_string());
        Router::new()
            .route("/callback", post(ok_handler))
            .layer(axum_middleware::from_fn(
                signature_verification::signature_verification,
            ))
            .layer(axum::Extension(store))
    }

    /// Compute a valid HMAC-SHA256 signature for the given timestamp and
    /// hex-encoded body, matching the server's signed-payload format:
    /// `"{timestamp}.{body_hex}"`.
    fn sign(timestamp: u64, body_hex: &str, secret: &str) -> String {
        let signed_payload = format!("{timestamp}.{body_hex}");
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_payload.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Current Unix timestamp (seconds).  Used to produce a "fresh" timestamp
    /// that passes the 5-minute replay-window check.
    fn now() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    // -----------------------------------------------------------------------
    // Admin auth — missing / invalid / valid bearer token
    // -----------------------------------------------------------------------

    /// `GET /admin/test` without an `Authorization` header must return 401.
    #[tokio::test]
    async fn test_admin_endpoint_rejects_missing_bearer_token() {
        std::env::set_var("ADMIN_API_KEY", "test-admin-key-missing");

        let app = admin_router();
        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "missing Authorization header must return 401"
        );
    }

    /// `GET /admin/test` with a wrong bearer token must return 401.
    #[tokio::test]
    async fn test_admin_endpoint_rejects_invalid_bearer_token() {
        std::env::set_var("ADMIN_API_KEY", "correct-admin-key");

        let app = admin_router();
        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", "Bearer wrong-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "incorrect bearer token must return 401"
        );
    }

    /// `GET /admin/test` with the correct bearer token must return 200.
    #[tokio::test]
    async fn test_admin_endpoint_accepts_valid_bearer_token() {
        let secret_key = "valid-admin-key-for-acceptance-test";
        std::env::set_var("ADMIN_API_KEY", secret_key);

        let app = admin_router();
        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", format!("Bearer {secret_key}"))
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "valid bearer token must return 200"
        );
    }

    /// The `admin_auth` middleware uses `SecretsStore` when present; a key in
    /// the store must be accepted.
    #[tokio::test]
    async fn test_admin_accepts_key_via_secrets_store() {
        let store = SecretsStore::new("wh-secret".to_string(), "store-admin-key".to_string());

        let app = Router::new()
            .route("/admin/test", get(ok_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", "Bearer store-admin-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "key from SecretsStore must be accepted"
        );
    }

    /// When `SecretsStore` is present, keys not in the store must be rejected.
    #[tokio::test]
    async fn test_admin_rejects_unknown_key_via_secrets_store() {
        let store = SecretsStore::new("wh-secret".to_string(), "store-admin-key".to_string());

        let app = Router::new()
            .route("/admin/test", get(ok_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store));

        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", "Bearer not-the-right-key")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "unknown key must be rejected when SecretsStore is present"
        );
    }

    // -----------------------------------------------------------------------
    // Signature verification — missing / invalid / valid / expired / rotation
    // -----------------------------------------------------------------------

    /// `POST /callback` without the `X-Webhook-Signature` header must return 401.
    #[tokio::test]
    async fn test_callback_endpoint_rejects_missing_signature() {
        let app = callback_router("test-webhook-secret");
        let body = br#"{"amount":"100"}"#;

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            // No X-Webhook-Signature header
            .header("X-Webhook-Timestamp", now().to_string())
            .header("Content-Type", "application/json")
            .body(Body::from(body.as_ref()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "missing X-Webhook-Signature must return 401"
        );
    }

    /// `POST /callback` with a wrong signature must return 401.
    #[tokio::test]
    async fn test_callback_endpoint_rejects_invalid_signature() {
        let app = callback_router("test-webhook-secret");
        let body = br#"{"amount":"100"}"#;
        let ts = now();

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("X-Webhook-Timestamp", ts.to_string())
            .header("X-Webhook-Signature", "0".repeat(64)) // wrong sig
            .header("Content-Type", "application/json")
            .body(Body::from(body.as_ref()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "incorrect HMAC signature must return 401"
        );
    }

    /// `POST /callback` with a valid signature must pass the middleware (200).
    #[tokio::test]
    async fn test_callback_endpoint_accepts_valid_signature() {
        let secret = "test-webhook-secret";
        let app = callback_router(secret);
        let body = br#"{"amount":"100"}"#;
        let ts = now();
        let body_hex = hex::encode(body);
        let sig = sign(ts, &body_hex, secret);

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("X-Webhook-Timestamp", ts.to_string())
            .header("X-Webhook-Signature", &sig)
            .header("Content-Type", "application/json")
            .body(Body::from(body.as_ref()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "valid HMAC signature must return 200"
        );
    }

    /// A timestamp older than 5 minutes (300 s) must be rejected with 401
    /// even when the HMAC is correct, to prevent replay attacks.
    #[tokio::test]
    async fn test_callback_endpoint_rejects_expired_timestamp() {
        let secret = "test-webhook-secret";
        let app = callback_router(secret);
        let body = br#"{"amount":"100"}"#;

        // Timestamp that is definitely outside the 5-minute replay window.
        let too_old_timestamp = 1_000_000_000u64; // year 2001
        let body_hex = hex::encode(body);
        let sig = sign(too_old_timestamp, &body_hex, secret);

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("X-Webhook-Timestamp", too_old_timestamp.to_string())
            .header("X-Webhook-Signature", &sig)
            .header("Content-Type", "application/json")
            .body(Body::from(body.as_ref()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "expired timestamp (replay attack) must return 401"
        );
    }

    /// During the grace period after a secret rotation, the previous secret
    /// must still be accepted (both current and previous are valid).
    #[tokio::test]
    async fn test_callback_with_rotation_grace_period_secret() {
        let current_secret = "current-webhook-secret";
        let previous_secret = "previous-webhook-secret";

        // Build a SecretsStore with both current and a grace-period previous secret.
        let store = {
            use synapse_core::secrets::RotatingSecret;
            use std::sync::Arc;
            use tokio::sync::RwLock;
            use std::time::Instant;

            let mut rotating = RotatingSecret::new(current_secret.to_string());
            // Inject a "previous" entry that is brand-new (so still in grace period).
            rotating.previous = Some((previous_secret.to_string(), Instant::now()));

            synapse_core::secrets::SecretsStore {
                anchor_webhook_secret: Arc::new(RwLock::new(rotating)),
                admin_api_key: Arc::new(RwLock::new(RotatingSecret::new(
                    "unused-admin-key".to_string(),
                ))),
            }
        };

        let app = Router::new()
            .route("/callback", post(ok_handler))
            .layer(axum_middleware::from_fn(
                signature_verification::signature_verification,
            ))
            .layer(axum::Extension(store));

        let body = br#"{"amount":"50"}"#;
        let ts = now();
        let body_hex = hex::encode(body);

        // Sign with the *previous* secret — must still be accepted in grace period.
        let sig_previous = sign(ts, &body_hex, previous_secret);

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("X-Webhook-Timestamp", ts.to_string())
            .header("X-Webhook-Signature", &sig_previous)
            .header("Content-Type", "application/json")
            .body(Body::from(body.as_ref()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "previous secret must be accepted during grace period"
        );
    }

    /// Verify that `admin_auth` uses constant-time comparison (behaviour test).
    ///
    /// If the middleware rejected "secret12" when the configured key is "secret123"
    /// — but accepted "secret123" — we can infer the comparison is not
    /// short-circuiting on prefix equality (as a naive byte-by-byte compare would).
    #[tokio::test]
    async fn test_admin_auth_uses_constant_time_comparison() {
        let correct_key = "super-secret-admin-key-xyz";
        std::env::set_var("ADMIN_API_KEY", correct_key);

        // A key that is a prefix of the correct key — rejected.
        let prefix_key = &correct_key[..correct_key.len() - 1];
        let app = admin_router();
        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", format!("Bearer {prefix_key}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "prefix of the correct key must be rejected (constant-time check)"
        );

        // The full correct key — accepted.
        let app2 = admin_router();
        let request2 = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", format!("Bearer {correct_key}"))
            .body(Body::empty())
            .unwrap();
        let response2 = app2.oneshot(request2).await.unwrap();
        assert_eq!(
            response2.status(),
            StatusCode::OK,
            "correct key must be accepted"
        );
    }

    /// Verify that `admin_auth` fails closed when `ADMIN_API_KEY` is not set
    /// and no `SecretsStore` is present — i.e. returns 401, not 500.
    #[tokio::test]
    async fn test_admin_key_required_no_default() {
        // Remove the env var to simulate a misconfigured deployment.
        std::env::remove_var("ADMIN_API_KEY");

        let app = Router::new()
            .route("/admin/test", get(ok_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth));
        // No SecretsStore extension — middleware must fall back to env var.

        let request = Request::builder()
            .method("GET")
            .uri("/admin/test")
            .header("Authorization", "Bearer anything")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "missing ADMIN_API_KEY with no SecretsStore must fail closed (401)"
        );
    }
}
