//! HMAC signature verification for inbound anchor callback/webhook requests.
//!
//! Before this module was wired in, `callback_routes`/`webhook_routes` in
//! `src/lib.rs` were protected only by an IP allowlist, quota rate limiting,
//! and JSON-schema *shape* validation (`middleware::validate::validate_callback`
//! / `validate_webhook`) — despite a comment in `src/lib.rs` claiming these
//! routes "authenticate inbound anchor calls via HMAC signature validation."
//! No code anywhere on the live path ever read `anchor_webhook_secret` or
//! computed an HMAC. `src/cache/webhook.rs` already contained a correct,
//! fully unit-tested HMAC-SHA256 implementation (binds timestamp and body
//! together, so a captured signature can't be replayed with an updated
//! timestamp) with zero callers anywhere. This module is the missing caller.
//!
//! # Expected headers
//!
//! - `X-Webhook-Timestamp`: Unix seconds, must be within 5 minutes of now.
//! - `X-Webhook-Signature`: `sha256=<hex>`, HMAC-SHA256 over
//!   `{timestamp}.{body}` keyed by the current (or, during a rotation grace
//!   window, previous) `anchor_webhook_secret`.

use crate::cache::webhook::{validate_timestamp, verify_signature, WebhookSecurityError};
use crate::AppState;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

const TIMESTAMP_HEADER: &str = "X-Webhook-Timestamp";
const SIGNATURE_HEADER: &str = "X-Webhook-Signature";

/// Returns every secret value that should currently be accepted: the
/// rotation-aware values from `SecretsStore` when Vault-backed secrets are
/// configured, otherwise the plain `ANCHOR_WEBHOOK_SECRET` env var — mirrors
/// the fallback `middleware::auth::is_valid_admin_request` uses for the admin
/// API key.
async fn valid_secrets(state: &AppState) -> Vec<String> {
    if let Some(store) = &state.secrets_store {
        store.valid_webhook_secrets().await
    } else {
        vec![std::env::var("ANCHOR_WEBHOOK_SECRET").unwrap_or_default()]
    }
}

fn rejection(status: StatusCode, error: WebhookSecurityError) -> Response {
    tracing::warn!(error = %error, "webhook_signature: rejected inbound anchor request");
    (status, Json(json!({ "error": error.to_string() }))).into_response()
}

/// Verifies `X-Webhook-Timestamp` + `X-Webhook-Signature` against the
/// configured anchor webhook secret(s) before letting the request reach
/// schema validation or the handler. Shared by both `callback_routes` and
/// `webhook_routes` — the check itself is identical for both.
pub async fn verify_anchor_signature(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next<Body>,
) -> Response {
    let timestamp = match req
        .headers()
        .get(TIMESTAMP_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => v.to_string(),
        None => {
            return rejection(
                StatusCode::UNAUTHORIZED,
                WebhookSecurityError::InvalidTimestamp,
            )
        }
    };

    let signature = match req
        .headers()
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(v) => v.to_string(),
        None => {
            return rejection(
                StatusCode::UNAUTHORIZED,
                WebhookSecurityError::MissingSignature,
            )
        }
    };

    if let Err(e) = validate_timestamp(&timestamp) {
        return rejection(StatusCode::UNAUTHORIZED, e);
    }

    let (parts, body) = req.into_parts();
    let bytes = match hyper::body::to_bytes(body).await {
        Ok(b) => b,
        Err(_) => {
            return rejection(
                StatusCode::BAD_REQUEST,
                WebhookSecurityError::InvalidSignature,
            )
        }
    };

    let secrets = valid_secrets(&state).await;
    let verified = secrets
        .iter()
        .any(|secret| verify_signature(secret.as_bytes(), &timestamp, &bytes, &signature).is_ok());

    if !verified {
        return rejection(
            StatusCode::UNAUTHORIZED,
            WebhookSecurityError::InvalidSignature,
        );
    }

    let req = Request::from_parts(parts, Body::from(bytes.to_vec()));
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tower::ServiceExt;

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    async fn test_app(secret: &str) -> Router {
        std::env::set_var("ANCHOR_WEBHOOK_SECRET", secret);
        let state = AppState::test_new(
            &std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse".into()),
        )
        .await;
        Router::new()
            .route("/callback", post(|| async { StatusCode::OK }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                verify_anchor_signature,
            ))
            .with_state(state)
    }

    #[tokio::test]
    #[ignore = "Requires DATABASE_URL"]
    async fn rejects_missing_signature_header() {
        let app = test_app("s3cr3t").await;
        let req = Request::builder()
            .method("POST")
            .uri("/callback")
            .header(TIMESTAMP_HEADER, now_secs().to_string())
            .body(Body::from("{}"))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[ignore = "Requires DATABASE_URL"]
    async fn rejects_replay_with_updated_timestamp() {
        // The exact attack Part C describes: capture a valid (signature,
        // body) pair, then replay it with a freshly updated timestamp. This
        // must fail because the signature was computed over the *original*
        // timestamp, not the new one — proving the timestamp is genuinely
        // bound into the signature on the live path now.
        let app = test_app("s3cr3t").await;
        let body = b"{\"stellar_account\":\"G...\"}";
        let original_ts = (now_secs() - 120).to_string();
        let sig = sign("s3cr3t", &original_ts, body);

        let replayed_ts = now_secs().to_string();
        let req = Request::builder()
            .method("POST")
            .uri("/callback")
            .header(TIMESTAMP_HEADER, replayed_ts)
            .header(SIGNATURE_HEADER, sig)
            .body(Body::from(body.to_vec()))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    #[ignore = "Requires DATABASE_URL"]
    async fn accepts_valid_signature() {
        let app = test_app("s3cr3t").await;
        let body = b"{\"stellar_account\":\"G...\"}";
        let ts = now_secs().to_string();
        let sig = sign("s3cr3t", &ts, body);

        let req = Request::builder()
            .method("POST")
            .uri("/callback")
            .header(TIMESTAMP_HEADER, ts)
            .header(SIGNATURE_HEADER, sig)
            .body(Body::from(body.to_vec()))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
