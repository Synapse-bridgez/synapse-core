use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use bytes::Bytes;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::secrets::SecretsStore;

type HmacSha256 = Hmac<Sha256>;

/// Maximum age of webhook (in seconds). Set to 5 minutes.
const MAX_TIMESTAMP_AGE_SECS: u64 = 300;

/// Signature verification middleware for webhook callbacks.
/// Expects:
/// - `X-Webhook-Timestamp` header (Unix seconds)
/// - `X-Webhook-Signature` header (hex-encoded HMAC-SHA256)
///
/// Verifies the signature against all currently-valid secrets from SecretsStore,
/// enforces the timestamp window to prevent replay attacks, and reconstructs
/// the request body for downstream handlers.
pub async fn signature_verification(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, StatusCode> {
    let source_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let secrets_store = match req.extensions().get::<SecretsStore>() {
        Some(store) => store.clone(),
        None => {
            tracing::error!("signature_verification: SecretsStore extension not found");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    let timestamp = match extract_timestamp(&req) {
        Ok(ts) => ts,
        Err(status) => {
            tracing::warn!(
                source_ip = %source_ip,
                "signature_verification: invalid or missing X-Webhook-Timestamp"
            );
            return Err(status);
        }
    };

    // Check timestamp is within acceptable window
    if let Err(status) = validate_timestamp(timestamp) {
        tracing::warn!(
            source_ip = %source_ip,
            "signature_verification: timestamp outside replay window"
        );
        return Err(status);
    }

    let provided_signature = match extract_signature(&req) {
        Some(sig) => sig,
        None => {
            tracing::warn!(
                source_ip = %source_ip,
                "signature_verification: missing X-Webhook-Signature header"
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    // Read the entire body into memory to compute signature
    let (parts, body) = req.into_parts();
    let body_bytes = match read_body_bytes(body).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "signature_verification: failed to read request body");
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    // Construct the signed payload: "{timestamp}.{body_bytes_hex}"
    let body_hex = hex::encode(&body_bytes);
    let signed_payload = format!("{}.{}", timestamp, body_hex);

    let valid_secrets = secrets_store.valid_webhook_secrets().await;
    let signature_valid = valid_secrets
        .iter()
        .any(|secret| verify_signature(&provided_signature, &signed_payload, secret));

    if !signature_valid {
        tracing::warn!(
            source_ip = %source_ip,
            "signature_verification: HMAC verification failed"
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Reconstruct request with the body we read
    let reconstructed_req = Request::from_parts(parts, Body::from(body_bytes));
    Ok(next.run(reconstructed_req).await)
}

/// Read request body into bytes. Works with axum 0.6.
async fn read_body_bytes(body: Body) -> Result<Bytes, String> {
    hyper::body::to_bytes(body).await.map_err(|e| e.to_string())
}

/// Extract timestamp from `X-Webhook-Timestamp` header and parse as u64.
fn extract_timestamp(req: &Request<Body>) -> Result<u64, StatusCode> {
    req.headers()
        .get("X-Webhook-Timestamp")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(StatusCode::BAD_REQUEST)
}

/// Validate that timestamp is within acceptable window to prevent replay attacks.
fn validate_timestamp(timestamp: u64) -> Result<(), StatusCode> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .as_secs();

    let age = now.saturating_sub(timestamp);
    if age > MAX_TIMESTAMP_AGE_SECS {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(())
}

/// Extract signature from `X-Webhook-Signature` header.
fn extract_signature(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get("X-Webhook-Signature")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

/// Verify HMAC-SHA256 signature using constant-time comparison.
fn verify_signature(provided_hex: &str, signed_payload: &str, secret: &str) -> bool {
    use subtle::ConstantTimeEq;

    let expected = match compute_hmac(signed_payload, secret) {
        Ok(h) => h,
        Err(_) => return false,
    };

    // Constant-time comparison using subtle
    match hex::decode(provided_hex) {
        Ok(provided_bytes) => {
            if provided_bytes.len() != expected.len() {
                return false;
            }
            provided_bytes.ct_eq(&expected[..]).into()
        }
        Err(_) => false,
    }
}

/// Compute HMAC-SHA256 of the signed payload using the secret.
fn compute_hmac(signed_payload: &str, secret: &str) -> Result<Bytes, String> {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| "invalid HMAC key length")?;
    mac.update(signed_payload.as_bytes());
    Ok(Bytes::from(mac.finalize().into_bytes().to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::Mac;

    #[test]
    fn test_verify_signature_valid() {
        let secret = "test-secret";
        let timestamp = 1234567890u64;
        let body = b"test body";
        let body_hex = hex::encode(body);
        let signed_payload = format!("{}.{}", timestamp, body_hex);

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_payload.as_bytes());
        let expected_sig = hex::encode(mac.finalize().into_bytes());

        assert!(verify_signature(&expected_sig, &signed_payload, secret));
    }

    #[test]
    fn test_verify_signature_invalid() {
        let secret = "test-secret";
        let timestamp = 1234567890u64;
        let body = b"test body";
        let body_hex = hex::encode(body);
        let signed_payload = format!("{}.{}", timestamp, body_hex);

        let bad_sig = "0".repeat(64); // Wrong signature
        assert!(!verify_signature(&bad_sig, &signed_payload, secret));
    }

    #[test]
    fn test_verify_signature_wrong_secret() {
        let secret = "test-secret";
        let wrong_secret = "wrong-secret";
        let timestamp = 1234567890u64;
        let body = b"test body";
        let body_hex = hex::encode(body);
        let signed_payload = format!("{}.{}", timestamp, body_hex);

        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_payload.as_bytes());
        let sig = hex::encode(mac.finalize().into_bytes());

        assert!(!verify_signature(&sig, &signed_payload, wrong_secret));
    }

    #[test]
    fn test_compute_hmac() {
        let secret = "test-secret";
        let payload = "test.payload";

        let result = compute_hmac(payload, secret);
        assert!(result.is_ok());

        let bytes = result.unwrap();
        assert_eq!(bytes.len(), 32); // SHA256 = 32 bytes
    }
}
