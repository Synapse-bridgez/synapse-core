use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::secrets::SecretsStore;

/// Admin auth middleware that accepts all currently-valid API keys (supports secret rotation).
/// If a `SecretsStore` extension is present on the request, it checks all valid keys
/// (current + grace-period previous). Falls back to the `ADMIN_API_KEY` env var otherwise.
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
        if valid_keys.iter().any(|k| k == &provided) {
            return Ok(next.run(req).await);
        }
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Fallback: plain env var (no Vault / rotation).
    let admin_api_key =
        std::env::var("ADMIN_API_KEY").unwrap_or_else(|_| "admin-secret-key".to_string());

    if provided == admin_api_key {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
