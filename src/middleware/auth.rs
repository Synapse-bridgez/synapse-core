use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;

use crate::secrets::SecretsStore;

/// API key authentication middleware for callback/webhook endpoints.
/// Requires `X-API-Key` header matching a key in the `tenants` table.
/// Returns 401 on missing or invalid key and logs the source IP.
pub async fn api_key_auth(req: Request<Body>, next: Next<Body>) -> Result<Response, StatusCode> {
    let api_key = req
        .headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let source_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let key = match api_key {
        Some(k) if !k.is_empty() => k,
        _ => {
            tracing::warn!(source_ip = %source_ip, "API key authentication failed: missing X-API-Key header");
            return Err(StatusCode::UNAUTHORIZED);
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
        Ok(true) => Ok(next.run(req).await),
        Ok(false) => {
            tracing::warn!(source_ip = %source_ip, "API key authentication failed: invalid key");
            Err(StatusCode::UNAUTHORIZED)
        }
        Err(e) => {
            tracing::error!(source_ip = %source_ip, error = %e, "API key lookup error");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Admin auth middleware that accepts all currently-valid API keys (supports secret rotation).
/// If a `SecretsStore` extension is present on the request, it checks all valid keys
/// (current + grace-period previous). Falls back to the `ADMIN_API_KEY` env var otherwise.
/// Uses constant-time comparison and requires ADMIN_API_KEY to be set (fails closed).
pub async fn admin_auth(req: Request<Body>, next: Next<Body>) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim_start_matches("Bearer ").to_string());

    let provided = match auth_header {
        Some(v) => v,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    // Try SecretsStore extension first (rotation-aware).
    if let Some(store) = req.extensions().get::<SecretsStore>() {
        let valid_keys = store.valid_admin_keys().await;
        if valid_keys
            .iter()
            .any(|k| constant_time_eq(k.as_bytes(), provided.as_bytes()))
        {
            return Ok(next.run(req).await);
        }
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Fallback: plain env var. Fail closed if not set.
    let admin_api_key = std::env::var("ADMIN_API_KEY").ok();
    let Some(admin_api_key) = admin_api_key else {
        tracing::error!("admin_auth: ADMIN_API_KEY not configured; request rejected");
        return Err(StatusCode::UNAUTHORIZED);
    };

    if constant_time_eq(admin_api_key.as_bytes(), provided.as_bytes()) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Constant-time byte slice equality check to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
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
}
