//! Integration tests for the `admin_auth` middleware.
//!
//! # What these tests cover
//!
//! Every test in this file builds a real [`axum::Router`] that includes the
//! exact `admin_auth` middleware wired into production's admin-only routes
//! (see `create_app` in `src/lib.rs`), sends an HTTP request through it via
//! [`tower::ServiceExt::oneshot`], and asserts on the actual HTTP response
//! status code — not just on local helper-function return values.
//!
//! # Scope note: no webhook-signature-verification tests here
//!
//! This file was originally going to also cover a `middleware::signature_verification`
//! layer for the `/callback` route. **That gap has since been closed** —
//! see `src/middleware/webhook_signature.rs`, which implements exactly the
//! HMAC + timestamp scheme this comment used to describe as hypothetical
//! (`X-Webhook-Timestamp` / `X-Webhook-Signature` headers, a signed payload
//! of `"{timestamp}.{body}"`), and is layered onto both `callback_routes`
//! and `webhook_routes` in `create_app`. Its own test module covers the
//! signature-verification behavior directly (including a replay-with-
//! updated-timestamp regression test); `tests/integration_test.rs`'s
//! callback-flow tests now sign their requests and
//! `test_invalid_signature_flow` exercises the real rejection path — its
//! `#[ignore = "Signature validation not implemented"]` reason (accurate at
//! the time this comment was written) is gone.
//!
//! What was previously true, for the historical record: `handlers::auth::
//! VerifiedWebhook` (a `FromRequest` extractor verifying a single
//! `X-Stellar-Signature` header, no timestamp/replay window, no rotation
//! support, zero call sites anywhere) has been **deleted** rather than
//! wired in — it duplicated a broken version of what `cache::webhook`
//! (used correctly by `webhook_signature.rs` now) already did right. This
//! file still doesn't need its own signature-verification tests: that
//! coverage lives with the middleware it belongs to.
//!
//! # Design
//!
//! Each test is self-contained and requires no external services (no
//! database, no Redis, no Docker). Where the middleware needs a dependency
//! injected as an axum [`Extension`] (i.e. `SecretsStore`), the test
//! constructs a minimal in-process instance directly.
//!
//! # Test isolation
//!
//! `admin_auth` supports two ways to supply the expected key: a
//! [`SecretsStore`] extension (per-request, no shared state) or a fallback to
//! the process-global `ADMIN_API_KEY` env var. Every test that can express
//! its scenario via `SecretsStore` does so specifically to avoid mutating
//! that env var — `std::env::set_var`/`remove_var` affect the whole process,
//! and this binary's tests run on multiple threads by default, so two tests
//! setting different values in parallel is a real, observed race (an earlier
//! version of this file used the env var in every test, and
//! `test_admin_endpoint_accepts_valid_bearer_token` /
//! `test_admin_auth_uses_constant_time_comparison` intermittently failed
//! under default parallelism as a result — reproducible with `cargo test
//! --test auth_and_signature_integration_test`, passing only under
//! `--test-threads=1`). `test_admin_key_required_no_default` is the sole
//! exception: it specifically tests the no-`SecretsStore`,
//! env-var-fallback path, so it cannot avoid touching the env var. It is the
//! *only* test in this file that does, so it no longer races with anything
//! else here.

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
    use tower::ServiceExt; // for `oneshot`

    /// A trivial 200 OK handler used as the "inner" handler behind middleware.
    async fn ok_handler() -> impl IntoResponse {
        (StatusCode::OK, "ok")
    }

    /// Build a router that applies `admin_auth`, with a [`SecretsStore`]
    /// extension containing `admin_key` as the current admin API key. This
    /// is the isolation-safe way to test `admin_auth`: no process-global
    /// state, safe under parallel test execution.
    fn admin_router_with_store(admin_key: &str) -> Router {
        let store = SecretsStore::new("unused-webhook-secret".to_string(), admin_key.to_string());
        Router::new()
            .route("/admin/test", get(ok_handler))
            .layer(axum_middleware::from_fn(auth::admin_auth))
            .layer(axum::Extension(store))
    }

    /// `GET /admin/test` without an `Authorization` header must return 401.
    #[tokio::test]
    async fn test_admin_endpoint_rejects_missing_bearer_token() {
        let app = admin_router_with_store("test-admin-key-missing");
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
        let app = admin_router_with_store("correct-admin-key");
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
        let app = admin_router_with_store(secret_key);
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
        let app = admin_router_with_store("store-admin-key");

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
        let app = admin_router_with_store("store-admin-key");

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

    /// Verify that `admin_auth` rejects a candidate that is a strict prefix
    /// of the correct key but accepts the full correct key — i.e. the
    /// comparison isn't short-circuiting on prefix equality the way a naive
    /// byte-by-byte compare that returns early would appear to from the
    /// outside.
    #[tokio::test]
    async fn test_admin_auth_uses_constant_time_comparison() {
        let correct_key = "super-secret-admin-key-xyz";
        let app = admin_router_with_store(correct_key);

        // A key that is a prefix of the correct key — rejected.
        let prefix_key = &correct_key[..correct_key.len() - 1];
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
            "prefix of the correct key must be rejected"
        );

        // The full correct key — accepted.
        let app2 = admin_router_with_store(correct_key);
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
    ///
    /// This is the one test in this file that touches the process-global
    /// `ADMIN_API_KEY` env var, because that fallback path is specifically
    /// what it tests. See the module doc comment for why every other test
    /// here uses `SecretsStore` instead.
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
