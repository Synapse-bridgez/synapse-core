//! Client-side webhook signature verification helper.
//!
//! Webhook *receivers* (integrators consuming Synapse webhook deliveries)
//! need to verify that an inbound payload was genuinely signed by the
//! server. This module provides [`verify_webhook_signature`], which mirrors
//! the server-side signing scheme implemented in
//! `src/middleware/webhook_signature.rs` (backed by `src/cache/webhook.rs`)
//! bit-for-bit:
//!
//! - `X-Webhook-Timestamp`: Unix seconds, must be within the replay window
//!   of now (see [`REPLAY_WINDOW_SECS`]).
//! - `X-Webhook-Signature`: `sha256=<hex>`, HMAC-SHA256 over
//!   `{timestamp}.{body}` keyed by the tenant's signing secret.
//!
//! The signature check uses a constant-time comparison so that a subtle
//! timing side-channel cannot be used to forge a signature byte-by-byte.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

/// Maximum age (in seconds) of a webhook timestamp before it is rejected as
/// a replay. Matches the server-side replay-acceptance window.
pub const REPLAY_WINDOW_SECS: u64 = 300;

/// Errors returned by [`verify_webhook_signature`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// The `X-Webhook-Timestamp` header was missing or not a valid integer.
    #[error("invalid or missing webhook timestamp")]
    InvalidTimestamp,
    /// The `X-Webhook-Signature` header was missing or malformed.
    #[error("missing or malformed webhook signature")]
    MissingSignature,
    /// The timestamp was outside the replay-acceptance window.
    #[error("webhook timestamp outside replay window")]
    TimestampOutOfWindow,
    /// The signature did not match the expected HMAC.
    #[error("webhook signature verification failed")]
    InvalidSignature,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verifies an inbound webhook payload against a tenant's signing secret.
///
/// `signature_header` is the raw value of the `X-Webhook-Signature` header
/// (e.g. `sha256=...`). `payload` is the raw request body bytes. The
/// timestamp is parsed from the `X-Webhook-Timestamp` header value passed as
/// `timestamp_header`.
///
/// Returns `Ok(())` when the signature is valid and the timestamp is within
/// the replay window, otherwise an [`Error`].
pub fn verify_webhook_signature(
    payload: &[u8],
    timestamp_header: &str,
    signature_header: &str,
    secret: &[u8],
) -> Result<(), Error> {
    let timestamp: u64 = timestamp_header
        .trim()
        .parse()
        .map_err(|_| Error::InvalidTimestamp)?;

    let now = now_secs();
    // Reject timestamps too far in the past (replay) or too far in the
    // future (clock skew / pre-signed forgery).
    if timestamp > now.saturating_add(REPLAY_WINDOW_SECS)
        || now.saturating_sub(timestamp) > REPLAY_WINDOW_SECS
    {
        return Err(Error::TimestampOutOfWindow);
    }

    let provided = signature_header
        .strip_prefix("sha256=")
        .ok_or(Error::MissingSignature)?;
    let provided_bytes = hex::decode(provided).map_err(|_| Error::MissingSignature)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| Error::InvalidSignature)?;
    mac.update(timestamp_header.trim().as_bytes());
    mac.update(b".");
    mac.update(payload);
    let expected = mac.finalize().into_bytes();

    if expected.as_slice().ct_eq(provided_bytes.as_slice()).into() {
        Ok(())
    } else {
        Err(Error::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_valid_signature() {
        let secret = b"s3cr3t";
        let body = b"{\"stellar_account\":\"G...\"}";
        let ts = now_secs().to_string();
        let sig = sign(secret, &ts, body);
        assert!(verify_webhook_signature(body, &ts, &sig, secret).is_ok());
    }

    #[test]
    fn rejects_tampered_payload() {
        let secret = b"s3cr3t";
        let body = b"{\"stellar_account\":\"G...\"}";
        let ts = now_secs().to_string();
        let sig = sign(secret, &ts, body);
        let tampered = b"{\"stellar_account\":\"GATTACKER\"}";
        assert_eq!(
            verify_webhook_signature(tampered, &ts, &sig, secret),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn rejects_replayed_stale_timestamp() {
        let secret = b"s3cr3t";
        let body = b"{}";
        let stale_ts = (now_secs() - REPLAY_WINDOW_SECS - 60).to_string();
        let sig = sign(secret, &stale_ts, body);
        assert_eq!(
            verify_webhook_signature(body, &stale_ts, &sig, secret),
            Err(Error::TimestampOutOfWindow)
        );
    }

    #[test]
    fn rejects_replay_with_updated_timestamp() {
        let secret = b"s3cr3t";
        let body = b"{}";
        let original_ts = (now_secs() - 120).to_string();
        let sig = sign(secret, &original_ts, body);
        let replayed_ts = now_secs().to_string();
        assert_eq!(
            verify_webhook_signature(body, &replayed_ts, &sig, secret),
            Err(Error::InvalidSignature)
        );
    }

    #[test]
    fn rejects_missing_signature() {
        let secret = b"s3cr3t";
        let ts = now_secs().to_string();
        assert_eq!(
            verify_webhook_signature(b"{}", &ts, "", secret),
            Err(Error::MissingSignature)
        );
    }
}
