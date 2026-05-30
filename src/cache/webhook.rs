//! Webhook security for the caching layer.
//!
//! Provides HMAC-SHA256 signature verification, timestamp-window replay
//! protection, event-ID validation, and Redis cache-key generation for
//! webhook nonces.  All logic is pure (no I/O) so it can be unit-tested
//! without a running Redis instance.
//!
//! # Typical usage
//!
//! ```ignore
//! use synapse_core::cache::webhook::{verify_signature, validate_timestamp, validate_event_id, replay_cache_key};
//!
//! // 1. Reject replays: timestamp must be within 5 minutes of now.
//! let ts = validate_timestamp(&headers["x-webhook-timestamp"])?;
//!
//! // 2. Verify HMAC signature before touching the body.
//! verify_signature(secret, &ts.to_string(), body_bytes, &headers["x-webhook-signature"])?;
//!
//! // 3. Build the Redis nonce key and SET NX to detect duplicates.
//! validate_event_id(&event_id)?;
//! let nonce_key = replay_cache_key("stripe", &event_id)?;
//! // … SET NX nonce_key "1" EX 86400 in Redis …
//! ```

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::validation::{CacheValidator, ValidationError};

type HmacSha256 = Hmac<Sha256>;

/// Timestamps older or newer than this many seconds relative to `now` are
/// rejected to prevent replay attacks.
const MAX_TIMESTAMP_SKEW_SECS: u64 = 300;

/// Maximum length of a webhook event ID used as a cache key component.
const MAX_EVENT_ID_LEN: usize = 128;

/// Errors produced by webhook security checks.
#[derive(Debug, thiserror::Error)]
pub enum WebhookSecurityError {
    #[error("missing X-Webhook-Signature header")]
    MissingSignature,
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("webhook timestamp is missing or non-numeric")]
    InvalidTimestamp,
    #[error("webhook timestamp is outside the acceptable window (replay protection)")]
    TimestampOutOfRange,
    #[error("duplicate event ID: replay detected")]
    ReplayDetected,
    #[error("webhook HMAC secret is not configured")]
    SecretNotConfigured,
    #[error("cache key error: {0}")]
    CacheKey(#[from] ValidationError),
    #[error("invalid event ID: {0}")]
    InvalidEventId(String),
}

/// Verifies an HMAC-SHA256 webhook signature.
///
/// # Expected signature format
///
/// `sha256=<lowercase-hex-digest>` — the same convention used by Stripe,
/// GitHub, and many other webhook providers.
///
/// # Signed payload
///
/// The HMAC is computed over `{timestamp}.{body}` so that an attacker cannot
/// replay a valid body with an updated timestamp without knowledge of the
/// secret.
///
/// # Timing safety
///
/// The hex-digest comparison is performed in constant time via
/// [`constant_time_eq`] to prevent timing-oracle attacks.
///
/// # Errors
///
/// Returns [`WebhookSecurityError::InvalidSignature`] if the prefix is wrong
/// or the digest does not match. Returns
/// [`WebhookSecurityError::SecretNotConfigured`] if `secret` is empty.
pub fn verify_signature(
    secret: &[u8],
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> Result<(), WebhookSecurityError> {
    if secret.is_empty() {
        return Err(WebhookSecurityError::SecretNotConfigured);
    }

    let hex_sig = signature
        .strip_prefix("sha256=")
        .ok_or(WebhookSecurityError::InvalidSignature)?;

    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| WebhookSecurityError::SecretNotConfigured)?;

    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);

    let expected = mac.finalize().into_bytes();
    let expected_hex = hex::encode(expected);

    if !constant_time_eq(hex_sig.as_bytes(), expected_hex.as_bytes()) {
        return Err(WebhookSecurityError::InvalidSignature);
    }

    Ok(())
}

/// Validates a Unix-second timestamp string against the current wall clock.
///
/// Timestamps more than [`MAX_TIMESTAMP_SKEW_SECS`] (300 s) in the past or
/// future are rejected to limit the replay window.
///
/// Returns the parsed timestamp on success so callers can embed it in the
/// signed payload without re-parsing.
///
/// # Errors
///
/// - [`WebhookSecurityError::InvalidTimestamp`] — not a valid integer.
/// - [`WebhookSecurityError::TimestampOutOfRange`] — outside the 5-minute window.
pub fn validate_timestamp(timestamp_str: &str) -> Result<u64, WebhookSecurityError> {
    let ts: u64 = timestamp_str
        .parse()
        .map_err(|_| WebhookSecurityError::InvalidTimestamp)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    if now.abs_diff(ts) > MAX_TIMESTAMP_SKEW_SECS {
        return Err(WebhookSecurityError::TimestampOutOfRange);
    }

    Ok(ts)
}

/// Validates a webhook event ID for safe use as a Redis cache-key component.
///
/// Event IDs must:
/// - Be non-empty and at most [`MAX_EVENT_ID_LEN`] (128) characters.
/// - Contain only ASCII alphanumeric characters, hyphens (`-`), underscores
///   (`_`), or colons (`:`).
///
/// These constraints are intentionally narrow to prevent Redis key injection
/// and cache-key collisions.
///
/// # Errors
///
/// Returns [`WebhookSecurityError::InvalidEventId`] with a descriptive message.
pub fn validate_event_id(event_id: &str) -> Result<(), WebhookSecurityError> {
    if event_id.is_empty() || event_id.len() > MAX_EVENT_ID_LEN {
        return Err(WebhookSecurityError::InvalidEventId(format!(
            "event_id must be 1–{MAX_EVENT_ID_LEN} characters"
        )));
    }
    if !event_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return Err(WebhookSecurityError::InvalidEventId(
            "event_id contains characters outside [A-Za-z0-9\\-_:]".to_string(),
        ));
    }
    Ok(())
}

/// Builds the Redis key used for webhook nonce / replay detection.
///
/// Keys are scoped by `source` (e.g. `"stripe"`, `"github"`) so that event
/// IDs from different providers cannot collide in a shared Redis keyspace.
///
/// The resulting key passes through [`CacheValidator::validate_key`] to
/// guarantee it meets Redis key-safety constraints before it is returned.
///
/// # Format
///
/// `webhook:nonce:{source}:{event_id}`
///
/// # Errors
///
/// Returns [`WebhookSecurityError::CacheKey`] if the composed key fails
/// validation (e.g. the source contains invalid characters).
pub fn replay_cache_key(source: &str, event_id: &str) -> Result<String, WebhookSecurityError> {
    let key = format!("webhook:nonce:{source}:{event_id}");
    CacheValidator::validate_key(&key)?;
    Ok(key)
}

/// Constant-time byte-slice comparison.
///
/// Returns `false` immediately when lengths differ (this does not leak
/// content information). For equal-length slices the comparison runs in
/// time proportional to the length regardless of where bytes first diverge.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn make_sig(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b".");
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    // ── verify_signature ─────────────────────────────────────────────────────

    #[test]
    fn verify_signature_accepts_valid() {
        let secret = b"test-secret";
        let ts = now_secs().to_string();
        let body = b"{\"event\":\"payment.completed\"}";
        let sig = make_sig(secret, &ts, body);
        assert!(verify_signature(secret, &ts, body, &sig).is_ok());
    }

    #[test]
    fn verify_signature_rejects_wrong_secret() {
        let ts = now_secs().to_string();
        let body = b"payload";
        let sig = make_sig(b"correct", &ts, body);
        assert!(verify_signature(b"wrong", &ts, body, &sig).is_err());
    }

    #[test]
    fn verify_signature_rejects_tampered_body() {
        let secret = b"s3cr3t";
        let ts = now_secs().to_string();
        let sig = make_sig(secret, &ts, b"original");
        assert!(verify_signature(secret, &ts, b"tampered", &sig).is_err());
    }

    #[test]
    fn verify_signature_rejects_missing_prefix() {
        let secret = b"s3cr3t";
        let ts = now_secs().to_string();
        let body = b"body";
        // Provide raw hex without `sha256=` prefix.
        let raw_hex = {
            let mut mac = HmacSha256::new_from_slice(secret).unwrap();
            mac.update(ts.as_bytes());
            mac.update(b".");
            mac.update(body);
            hex::encode(mac.finalize().into_bytes())
        };
        assert!(verify_signature(secret, &ts, body, &raw_hex).is_err());
    }

    #[test]
    fn verify_signature_rejects_empty_secret() {
        let ts = now_secs().to_string();
        assert!(verify_signature(b"", &ts, b"body", "sha256=abc").is_err());
    }

    // ── validate_timestamp ───────────────────────────────────────────────────

    #[test]
    fn validate_timestamp_accepts_current() {
        assert!(validate_timestamp(&now_secs().to_string()).is_ok());
    }

    #[test]
    fn validate_timestamp_rejects_old() {
        let old = (now_secs() - 600).to_string();
        assert!(validate_timestamp(&old).is_err());
    }

    #[test]
    fn validate_timestamp_rejects_future() {
        let future = (now_secs() + 600).to_string();
        assert!(validate_timestamp(&future).is_err());
    }

    #[test]
    fn validate_timestamp_rejects_non_numeric() {
        assert!(validate_timestamp("not-a-number").is_err());
    }

    // ── validate_event_id ────────────────────────────────────────────────────

    #[test]
    fn validate_event_id_accepts_valid() {
        assert!(validate_event_id("evt_abc-123_XYZ:ok").is_ok());
    }

    #[test]
    fn validate_event_id_rejects_empty() {
        assert!(validate_event_id("").is_err());
    }

    #[test]
    fn validate_event_id_rejects_too_long() {
        assert!(validate_event_id(&"a".repeat(MAX_EVENT_ID_LEN + 1)).is_err());
    }

    #[test]
    fn validate_event_id_accepts_max_length() {
        assert!(validate_event_id(&"a".repeat(MAX_EVENT_ID_LEN)).is_ok());
    }

    #[test]
    fn validate_event_id_rejects_special_chars() {
        assert!(validate_event_id("evt@bad!").is_err());
        assert!(validate_event_id("evt/path").is_err());
        assert!(validate_event_id("evt space").is_err());
    }

    // ── replay_cache_key ─────────────────────────────────────────────────────

    #[test]
    fn replay_cache_key_produces_expected_format() {
        let key = replay_cache_key("stripe", "evt_123").unwrap();
        assert_eq!(key, "webhook:nonce:stripe:evt_123");
    }

    // ── constant_time_eq ─────────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_equal_slices() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
    }

    #[test]
    fn constant_time_eq_different_content() {
        assert!(!constant_time_eq(b"abc", b"xyz"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn constant_time_eq_empty_slices() {
        assert!(constant_time_eq(b"", b""));
    }
}
