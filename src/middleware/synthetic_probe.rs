use crate::cache::webhook::{validate_timestamp, verify_signature};
use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

const MAX_SYNTHETIC_BODY_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct SyntheticProbe;

impl SyntheticProbe {
    pub const FLOW_LABEL: &'static str = "synthetic";

    pub const fn counts_toward_customer_traffic(self) -> bool {
        false
    }
}

pub fn verify_probe_request(secret: &[u8], timestamp: &str, body: &[u8], signature: &str) -> bool {
    validate_timestamp(timestamp).is_ok()
        && verify_signature(secret, timestamp, body, signature).is_ok()
}

pub fn is_allowed_graphql_probe(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("query").and_then(serde_json::Value::as_str).map(str::to_owned))
        .is_some_and(|query| query.trim() == "query SyntheticProbe { __typename }")
}

pub async fn validate_synthetic_graphql(req: Request<Body>, next: Next<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let body = match hyper::body::to_bytes(body).await {
        Ok(body) if body.len() <= MAX_SYNTHETIC_BODY_BYTES => body,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    if !is_allowed_graphql_probe(&body) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    next.run(Request::from_parts(parts, Body::from(body))).await
}

pub async fn authorize_synthetic_probe(req: Request<Body>, next: Next<Body>) -> Response {
    let secret = match std::env::var("SYNTHETIC_PROBE_SECRET") {
        Ok(secret) if !secret.is_empty() => secret,
        _ => {
            tracing::error!(synthetic_probe = true, "Synthetic probe secret is not configured");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let timestamp = match req
        .headers()
        .get("X-Synthetic-Probe-Timestamp")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value.to_owned(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let signature = match req
        .headers()
        .get("X-Synthetic-Probe-Signature")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value.to_owned(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let (mut parts, body) = req.into_parts();
    let body = match hyper::body::to_bytes(body).await {
        Ok(body) if body.len() <= MAX_SYNTHETIC_BODY_BYTES => body,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    if !verify_probe_request(secret.as_bytes(), &timestamp, &body, &signature) {
        tracing::warn!(synthetic_probe = true, "Rejected synthetic probe signature");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    parts.extensions.insert(SyntheticProbe);
    tracing::info!(synthetic_probe = true, "Authorized synthetic probe request");
    next.run(Request::from_parts(parts, Body::from(body))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn signed(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_current_signed_probe_and_rejects_tampering() {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        let body = br#"{"probe":true}"#;
        let signature = signed(b"test-secret", &timestamp, body);

        assert!(verify_probe_request(
            b"test-secret",
            &timestamp,
            body,
            &signature
        ));
        assert!(!verify_probe_request(
            b"test-secret",
            &timestamp,
            b"{\"probe\":false}",
            &signature
        ));
    }

    #[test]
    fn synthetic_probe_is_not_customer_traffic() {
        assert!(!SyntheticProbe.counts_toward_customer_traffic());
    }

    #[test]
    fn synthetic_graphql_allowlist_rejects_data_queries() {
        assert!(is_allowed_graphql_probe(
            br#"{"query":"query SyntheticProbe { __typename }"}"#
        ));
        assert!(!is_allowed_graphql_probe(
            br#"{"query":"{ transactions(limit: 1) { id } }"}"#
        ));
    }
}
