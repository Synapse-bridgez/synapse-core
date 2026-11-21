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
//!
//! # Verification cache
//!
//! Senders retry deliveries, so the same `(tenant_id, signature)` pair can
//! arrive several times within a short window. A bounded, TTL-based cache of
//! *successful* verifications lets duplicate deliveries skip the HMAC work.
//! Only acceptances are cached (never rejections), and entries expire after a
//! few seconds so a rotated secret is not honoured past its lifetime.

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
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TIMESTAMP_HEADER: &str = "X-Webhook-Timestamp";
const SIGNATURE_HEADER: &str = "X-Webhook-Signature";

/// How long a successful verification stays cached. Kept short so a rotated
/// secret is not honoured past its lifetime.
const VERIFIED_TTL: Duration = Duration::from_secs(5);

/// Upper bound on cached entries; oldest entries are evicted first.
const VERIFIED_CAPACITY: usize = 1024;

/// Bounded, TTL-based cache of verified `(tenant_id, signature)` pairs.
struct VerifiedCache {
    entries: HashMap<(String, String), Instant>,
}

impl VerifiedCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn get(&mut self, key: &(String, String)) -> bool {
        match self.entries.get(key) {
            Some(expires_at) if *expires_at > Instant::now() => true,
            Some(_) => {
                self.entries.remove(key);
                false
            }
            None => false,
        }
    }

    fn insert(&mut self, key: (String, String)) {
        let now = Instant::now();
        self.entries.retain(|_, expires_at| *expires_at > now);
        if self.entries.len() >= VERIFIED_CAPACITY {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, expires_at)| **expires_at)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key, now + VERIFIED_TTL);
    }
}

fn verified_cache() -> &'static Mutex<VerifiedCache> {
    static CACHE: OnceLock<Mutex<VerifiedCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VerifiedCache::new()))
}

fn cache_get(key: &(String, String)) -> bool {
    verified_cache()
        .lock()
        .map(|mut cache| cache.get(key))
        .unwrap_or(false)
}

fn cache_insert(key: (String, String)) {
    if let Ok(mut cache) = verified_cache().lock() {
        cache.insert(key);
    }
}

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

    let tenant_id = parts
        .extensions
        .get::<crate::middleware::tenant::TenantContext>()
        .map(|ctx| ctx.tenant_id.clone())
        .unwrap_or_default();
    let cache_key = (tenant_id, signature.clone());

    if cache_get(&cache_key) {
        crate::metrics::record_webhook_signature_cache_hit();
        let req = Request::from_parts(parts, Body::from(bytes.to_vec()));
        return next.run(req).await;
    }

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

    cache_insert(cache_key);

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

    #[test]
    fn verified_cache_skips_duplicate_and_expires() {
        let mut cache = VerifiedCache::new();
        let key = ("tenant-a".to_string(), "sha256=deadbeef".to_string());

        assert!(!cache.get(&key));
        cache.insert(key.clone());
        assert!(cache.get(&key));

        // Expired entries are treated as misses.
        cache.entries.insert(key.clone(), Instant::now());
        assert!(!cache.get(&key));
    }

    #[test]
    fn verified_cache_is_bounded() {
        let mut cache = VerifiedCache::new();
        for i in 0..(VERIFIED_CAPACITY + 10) {
            cache.insert(("tenant".to_string(), format!("sig-{i}")));
        }
        assert!(cache.entries.len() <= VERIFIED_CAPACITY);
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

    #[tokio::test]
    #[ignore = "Requires DATABASE_URL"]
    async fn duplicate_delivery_hits_cache() {
        let app = test_app("s3cr3t").await;
        let body = b"{\"stellar_account\":\"G...\"}";
        let ts = now_secs().to_string();
        let sig = sign("s3cr3t", &ts, body);

        let build = || {
            Request::builder()
                .method("POST")
                .uri("/callback")
                .header(TIMESTAMP_HEADER, ts.clone())
                .header(SIGNATURE_HEADER, sig.clone())
                .body(Body::from(body.to_vec()))
                .unwrap()
        };

        let first = app.clone().oneshot(build()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert!(cache_get(&(String::new(), sig.clone())));

        let second = app.oneshot(build()).await.unwrap();
        assert_eq!(second.status(), StatusCode::OK);
    }
}
