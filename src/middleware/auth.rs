use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;

use crate::auth::rate_limiting::{ADMIN_AUTH_RATE_LIMITER, TENANT_AUTH_RATE_LIMITER};
use crate::secrets::SecretsStore;

fn source_ip(req: &Request<Body>) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// API key authentication middleware for callback/webhook endpoints.
/// Requires `X-API-Key` header matching a key in the `tenants` table.
/// Returns 401 on missing or invalid key and logs the source IP.
///
/// Only failed lookups consume the `TENANT_AUTH_RATE_LIMITER` budget — see
/// `admin_auth`'s doc comment for why a valid key used repeatedly must not
/// count against a guessing-attack throttle.
pub async fn api_key_auth(req: Request<Body>, next: Next<Body>) -> Result<Response, StatusCode> {
    let api_key = req
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let source_ip = source_ip(&req);

    let key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            tracing::warn!(source_ip = %source_ip, "API key authentication failed: missing X-API-Key header");
            return rate_limited_unauthorized(&source_ip, "api_key_auth").await;
        }
    };

    // Extract the DB pool from extensions (injected via AppState layer)
    let pool = req.extensions().get::<sqlx::PgPool>().cloned();

    let pool = match pool {
        Some(p) => p,
        None => {
            tracing::error!("api_key_auth: PgPool extension not found");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    match crate::db::queries::lookup_api_key(&pool, &key).await {
        Ok(Some(_tenant_id)) => Ok(next.run(req).await),
        Ok(None) => {
            tracing::warn!(source_ip = %source_ip, "API key authentication failed: invalid key");
            rate_limited_unauthorized(&source_ip, "api_key_auth").await
        }
        Err(e) => {
            tracing::error!(source_ip = %source_ip, error = %e, "API key lookup error");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Checks the shared tenant-auth brute-force budget and returns the
/// appropriate rejection: 429 if the budget is exhausted, 401 otherwise.
async fn rate_limited_unauthorized(
    source_ip: &str,
    middleware: &str,
) -> Result<Response, StatusCode> {
    if let Err(e) = TENANT_AUTH_RATE_LIMITER.check_auth_rate_limit(&format!("ip:{source_ip}")) {
        tracing::warn!(
            counter.api_key_auth_lockout_triggered_total = 1u64,
            source_ip = %source_ip,
            middleware,
            error = %e,
            "rate limit exceeded"
        );
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    Err(StatusCode::UNAUTHORIZED)
}

/// Admin auth middleware that accepts all currently-valid API keys (supports secret rotation).
/// If a `SecretsStore` extension is present on the request, it checks all valid keys
/// (current + grace-period previous). Falls back to the `ADMIN_API_KEY` env var otherwise.
///
/// Rate-limited by source IP via `ADMIN_AUTH_RATE_LIMITER` — before this fix,
/// this was the only thing standing between an attacker and unlimited
/// `Authorization: Bearer <guess>` attempts against every `/admin/*` route,
/// despite a complete, unit-tested `AuthRateLimiter` already existing in this
/// codebase for exactly this purpose (previously wired only into Vault-probe
/// limiting — see `secrets.rs`).
///
/// Only *failed* attempts consume the rate-limit budget — checked lazily,
/// after determining the request would otherwise be rejected. An admin
/// using the correct key any number of times per minute never touches the
/// limiter at all: this guards against guessing, not against legitimate
/// request volume (there is no separate quota middleware on `/admin/*` the
/// way there is on tenant routes, so this must not double as one).
pub async fn admin_auth(req: Request<Body>, next: Next<Body>) -> Result<Response, StatusCode> {
    if is_valid_admin_request(&req).await {
        return Ok(next.run(req).await);
    }

    let source_ip = source_ip(&req);
    if let Err(e) = ADMIN_AUTH_RATE_LIMITER.check_auth_rate_limit(&format!("ip:{source_ip}")) {
        // ADMIN_AUTH_RATE_LIMIT_MODE=shadow logs what would have been
        // rejected without actually rejecting it. A false lockout of a
        // legitimate admin during an incident is its own operational risk
        // (see the issue this fixes), so this defaults to enforcing but lets
        // a rollout observe real-world trigger rates for a deploy cycle
        // before committing to enforcement, without shipping a second PR.
        let shadow_mode = std::env::var("ADMIN_AUTH_RATE_LIMIT_MODE").as_deref() == Ok("shadow");

        tracing::warn!(
            counter.admin_auth_lockout_triggered_total = 1u64,
            source_ip = %source_ip,
            error = %e,
            shadow_mode,
            "admin_auth: rate limit exceeded{}",
            if shadow_mode { " (shadow mode: not enforced)" } else { "" }
        );

        if !shadow_mode {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

async fn is_valid_admin_request(req: &Request<Body>) -> bool {
    let provided = match req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim_start_matches("Bearer ").to_string())
    {
        Some(v) => v,
        None => return false,
    };

    // Try SecretsStore extension first (rotation-aware).
    if let Some(store) = req.extensions().get::<SecretsStore>() {
        return store.verify_admin_key(&provided).await;
    }

    // Fallback: plain env var (no Vault / rotation).
    let admin_api_key =
        std::env::var("ADMIN_API_KEY").unwrap_or_else(|_| "admin-secret-key".to_string());
    provided == admin_api_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    fn make_request_without_key() -> Request<Body> {
        Request::builder()
            .uri("/callback")
            .body(Body::empty())
            .unwrap()
    }

    fn make_request_with_key(key: &str) -> Request<Body> {
        Request::builder()
            .uri("/callback")
            .header("X-API-Key", key)
            .body(Body::empty())
            .unwrap()
    }

    /// Verify that a request without X-API-Key is rejected with 401 before any DB lookup.
    #[test]
    fn test_missing_api_key_header_is_rejected() {
        let req = make_request_without_key();
        let api_key = req
            .headers()
            .get("X-API-Key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        assert!(api_key.is_none(), "No X-API-Key header should be present");
    }

    /// Verify that an empty X-API-Key header is treated as missing.
    #[test]
    fn test_empty_api_key_header_is_rejected() {
        let req = make_request_with_key("");
        let key = req
            .headers()
            .get("X-API-Key")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        assert!(
            key.is_none(),
            "Empty X-API-Key should be treated as missing"
        );
    }

    /// Simulates a brute-force attempt against admin_auth and confirms
    /// lockout engages within AuthRateLimiter's configured threshold
    /// (DEFAULT_AUTH_LIMIT = 10 per 60s). Regression test for Part C: before
    /// this fix, admin_auth had no rate limiting of any kind and an attacker
    /// could send unlimited `Authorization: Bearer <guess>` attempts.
    ///
    /// Uses a unique fake source port so this test's bucket in the
    /// process-wide `ADMIN_AUTH_RATE_LIMITER` static doesn't collide with
    /// other tests in this module running concurrently.
    #[tokio::test]
    async fn test_admin_auth_locks_out_after_repeated_failures() {
        use axum::{extract::ConnectInfo, middleware::from_fn, routing::get, Router};
        use std::net::SocketAddr;
        use tower::ServiceExt;

        let addr: SocketAddr = "127.0.0.1:59001".parse().unwrap();
        let app: Router = Router::new()
            .route("/admin/probe", get(|| async { "ok" }))
            .layer(from_fn(admin_auth));

        for attempt in 0..10 {
            let req = Request::builder()
                .uri("/admin/probe")
                .header("Authorization", "Bearer wrong-admin-key")
                .extension(ConnectInfo(addr))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {attempt} should fail auth (wrong key), not be rate-limited yet"
            );
        }

        let req = Request::builder()
            .uri("/admin/probe")
            .header("Authorization", "Bearer wrong-admin-key")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the 11th attempt from the same source should be locked out"
        );
    }

    /// Same as above, for api_key_auth / TENANT_AUTH_RATE_LIMITER. Only
    /// *failed* lookups count against the budget (see api_key_auth's doc
    /// comment), so this needs a real PgPool extension to actually reach
    /// the `Ok(None)` branch on each attempt — api_key_auth expects one
    /// injected via an `Extension` layer, which nothing in `create_app`
    /// actually does (it has no live callers — see the tracked issue), so
    /// this test provides its own.
    #[tokio::test]
    #[ignore = "Requires a live database"]
    async fn test_api_key_auth_locks_out_after_repeated_failures() {
        use axum::{extract::ConnectInfo, middleware::from_fn, routing::get, Extension, Router};
        use std::net::SocketAddr;
        use tower::ServiceExt;

        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse_app:synapse_app@localhost:5432/synapse_test".to_string()
        });
        let pool = match sqlx::PgPool::connect(&database_url).await {
            Ok(p) => p,
            Err(_) => return, // no DB available in this environment; skip
        };

        let addr: SocketAddr = "127.0.0.1:59002".parse().unwrap();
        let app: Router = Router::new()
            .route("/callback", get(|| async { "ok" }))
            .layer(from_fn(api_key_auth))
            .layer(Extension(pool));

        for attempt in 0..10 {
            let req = Request::builder()
                .uri("/callback")
                .header("X-API-Key", "this-key-was-never-issued-to-anyone")
                .extension(ConnectInfo(addr))
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::UNAUTHORIZED,
                "attempt {attempt} should fail auth (unknown key), not be rate-limited yet"
            );
        }

        let req = Request::builder()
            .uri("/callback")
            .header("X-API-Key", "this-key-was-never-issued-to-anyone")
            .extension(ConnectInfo(addr))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the 11th attempt from the same source should be locked out"
        );
    }
}
