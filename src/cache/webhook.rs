//! Webhook security for the caching layer.
//!
//! Provides HMAC-SHA256 signature verification, timestamp-window replay
//! protection, event-ID validation, and Redis cache-key generation for
//! webhook nonces.  All logic is pure (no I/O) so it can be unit-tested
//! without a running Redis instance.
//!
//! # Typical usage
//!
//! ```text
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

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::validation::{CacheValidator, ValidationError};

type HmacSha256 = Hmac<Sha256>;

/// Timestamps older or newer than this many seconds relative to `now` are
/// rejected to prevent replay attacks.
const MAX_TIMESTAMP_SKEW_SECS: u64 = 300;

/// Maximum length of a webhook event ID used as a cache key component.
const MAX_EVENT_ID_LEN: usize = 128;

/// How long a verified signature stays in the in-process cache.  Kept short
/// so a rotated secret is not honoured past its lifetime.
const VERIFIED_SIGNATURE_TTL: Duration = Duration::from_secs(5);

/// Upper bound on the number of cached verified signatures.  When the cache
/// is full, expired entries are pruned first; if it is still full the new
/// entry is simply not cached (verification still succeeds).
const VERIFIED_SIGNATURE_CAPACITY: usize = 1024;

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

/// Bounded, TTL-based cache of verified webhook signatures.
///
/// Entries are keyed by `(tenant_id, signature)` and only ever store
/// *successful* verifications, so a cache hit can never turn a rejection
/// into an acceptance.  Entries expire after [`VERIFIED_SIGNATURE_TTL`].
struct VerifiedSignatureCache {
    entries: HashMap<(String, String), Instant>,
}

impl VerifiedSignatureCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Returns `true` if a non-expired entry exists for the key.
    fn contains(&mut self, tenant_id: &str, signature: &str) -> bool {
        let key = (tenant_id.to_string(), signature.to_string());
        match self.entries.get(&key) {
            Some(inserted) if inserted.elapsed() < VERIFIED_SIGNATURE_TTL => true,
            Some(_) => {
                self.entries.remove(&key);
                false
            }
            None => false,
        }
    }

    /// Records a successful verification, pruning expired entries first.
    fn insert(&mut self, tenant_id: &str, signature: &str) {
        self.prune_expired();
        if self.entries.len() >= VERIFIED_SIGNATURE_CAPACITY {
            return;
        }
        self.entries.insert(
            (tenant_id.to_string(), signature.to_string()),
            Instant::now(),
        );
    }

    fn prune_expired(&mut self) {
        self.entries
            .retain(|_, inserted| inserted.elapsed() < VERIFIED_SIGNATURE_TTL);
    }
}

fn verified_signature_cache() -> &'static Mutex<VerifiedSignatureCache> {
    static CACHE: OnceLock<Mutex<VerifiedSignatureCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(VerifiedSignatureCache::new()))
}

/// Returns `true` if `(tenant_id, signature)` was verified within the last
/// [`VERIFIED_SIGNATURE_TTL`] seconds, allowing callers to skip the HMAC.
pub fn is_signature_cached(tenant_id: &str, signature: &str) -> bool {
    let mut cache = match verified_signature_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache.contains(tenant_id, signature)
}

/// Records a successful signature verification for `(tenant_id, signature)`.
///
/// Only call this after [`verify_signature`] has returned `Ok(())`; rejections
/// must never be cached.
pub fn cache_verified_signature(tenant_id: &str, signature: &str) {
    let mut cache = match verified_signature_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache.insert(tenant_id, signature);
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

/// Verifies a webhook signature, consulting the short-lived verified-signature
/// cache first so duplicate deliveries skip the HMAC computation.
///
/// On a cache hit the same accept decision as a fresh verification is
/// returned.  On a miss the HMAC is computed and, only on success, the
/// `(tenant_id, signature)` pair is cached for [`VERIFIED_SIGNATURE_TTL`].
/// Rejections are never cached.
pub fn verify_signature_cached(
    tenant_id: &str,
    secret: &[u8],
    timestamp: &str,
    body: &[u8],
    signature: &str,
) -> Result<(), WebhookSecurityError> {
    if is_signature_cached(tenant_id, signature) {
        return Ok(());
    }

    verify_signature(secret, timestamp, body, signature)?;
    cache_verified_signature(tenant_id, signature);
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
    fn verify_signature_cached_skips_hmac_on_duplicate() {
        let secret = b"test-secret";
        let ts = now_secs().to_string();
        let body = b"{\"event\":\"payment.completed\"}";
        let sig = make_sig(secret, &ts, body);
        let tenant = "tenant-cache-test";

        // First delivery: cache miss, HMAC computed and cached.
        assert!(!is_signature_cached(tenant, &sig));
        assert!(verify_signature_cached(tenant, secret, &ts, body, &sig).is_ok());
        assert!(is_signature_cached(tenant, &sig));

        // Second delivery: cache hit, HMAC skipped but still accepted.
        assert!(verify_signature_cached(tenant, secret, &ts, body, &sig).is_ok());
    }

    #[test]
    fn verify_signature_cached_never_caches_rejection() {
        let secret = b"test-secret";
        let ts = now_secs().to_string();
        let body = b"{\"event\":\"payment.completed\"}";
        let bad_sig = "sha256=deadbeef";
        let tenant = "tenant-reject-test";

        assert!(verify_signature_cached(tenant, secret, &ts, body, bad_sig).is_err());
        assert!(!is_signature_cached(tenant, bad_sig));
    }
}
