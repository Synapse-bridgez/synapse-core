//! Optimized rate limiting for the Auth module (vaultrs integration).
//!
//! Provides per-identity token-bucket rate limiting for authentication
//! operations, with input validation, metrics integration, and configurable
//! limits.
//!
//! # Design
//!
//! - Uses the same lock-free [`RateLimiter`] from [`crate::cache::rate_limiting`]
//!   so there is no duplicated token-bucket logic.
//! - Each identity (API key or IP address) gets its own bucket stored in a
//!   shared [`Arc<Mutex<HashMap>>`].  The `Mutex` is held only for the
//!   `HashMap` lookup/insert, not for the token acquisition itself, keeping
//!   contention minimal.
//! - Key validation runs *before* the bucket lookup so malformed keys never
//!   allocate a bucket entry.
//! - Metrics are recorded via [`AuthMetrics`] so auth dashboards reflect
//!   rate-limit activity alongside authentication outcomes.
//!
//! # Limits
//!
//! | Operation | Default limit | Window |
//! |-----------|--------------|--------|
//! | Auth attempts (per identity) | 10 req | 60 s |
//! | Vault health probes | 5 req | 60 s |
//!
//! Both are configurable via [`AuthRateLimitConfig`].
//!
//! # Security
//!
//! - Identity keys are validated (length + character allowlist) before use.
//! - Vault probe rate limiting prevents hammering the Vault endpoint during
//!   cascading failures.
//! - Exhausted callers receive a structured [`AuthError::RateLimited`] with a
//!   `retry_after_secs` hint derived from the token-bucket state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::auth::error::AuthError;
use crate::auth::metrics::AuthMetrics;
use crate::cache::rate_limiting::{RateLimitConfig, RateLimitStrategy, RateLimiter};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum auth attempts per identity per window.
const DEFAULT_AUTH_LIMIT: u32 = 10;

/// Default maximum vault health probe calls per window.
const DEFAULT_VAULT_PROBE_LIMIT: u32 = 5;

/// Default rate-limit window for auth operations.
const DEFAULT_AUTH_WINDOW: Duration = Duration::from_secs(60);

/// Maximum allowed length for an identity key (API key or IP string).
const MAX_IDENTITY_KEY_LEN: usize = 256;

/// Minimum allowed length for an identity key.
const MIN_IDENTITY_KEY_LEN: usize = 1;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for auth-layer rate limiting.
#[derive(Debug, Clone)]
pub struct AuthRateLimitConfig {
    /// Maximum authentication attempts per identity per window.
    pub auth_limit: u32,
    /// Maximum vault health probe calls per window.
    pub vault_probe_limit: u32,
    /// Duration of the rate-limit window.
    pub window: Duration,
}

impl Default for AuthRateLimitConfig {
    fn default() -> Self {
        Self {
            auth_limit: DEFAULT_AUTH_LIMIT,
            vault_probe_limit: DEFAULT_VAULT_PROBE_LIMIT,
            window: DEFAULT_AUTH_WINDOW,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validates an identity key before it is used as a rate-limit bucket key.
///
/// Accepts ASCII alphanumeric characters plus `-`, `_`, `.`, and `:` (the
/// colon allows `ip:1.2.3.4`-style prefixed keys).
///
/// # Errors
///
/// Returns [`AuthError::Validation`] with a descriptive message when the key
/// fails validation.
pub fn validate_identity_key(key: &str) -> Result<(), AuthError> {
    if key.is_empty() || key.len() < MIN_IDENTITY_KEY_LEN {
        return Err(AuthError::Validation(
            "identity key cannot be empty".to_string(),
        ));
    }
    if key.len() > MAX_IDENTITY_KEY_LEN {
        return Err(AuthError::Validation(format!(
            "identity key exceeds maximum length of {MAX_IDENTITY_KEY_LEN}"
        )));
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(AuthError::Validation(
            "identity key contains invalid characters".to_string(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AuthRateLimiter
// ---------------------------------------------------------------------------

/// Thread-safe, per-identity rate limiter for authentication operations.
///
/// Cloning is O(1) — all clones share the same bucket store and metrics.
#[derive(Clone)]
pub struct AuthRateLimiter {
    config: AuthRateLimitConfig,
    /// Per-identity auth buckets.
    auth_buckets: Arc<Mutex<HashMap<String, RateLimiter>>>,
    /// Single shared bucket for vault health probes.
    vault_bucket: RateLimiter,
    metrics: AuthMetrics,
}

impl AuthRateLimiter {
    /// Creates a new rate limiter with default configuration.
    pub fn new() -> Self {
        Self::with_config(AuthRateLimitConfig::default())
    }

    /// Creates a new rate limiter with custom configuration.
    pub fn with_config(config: AuthRateLimitConfig) -> Self {
        let vault_bucket = RateLimiter::with_config(RateLimitConfig {
            max_requests: config.vault_probe_limit,
            window: config.window,
            strategy: RateLimitStrategy::TokenBucket,
        });
        Self {
            config,
            auth_buckets: Arc::new(Mutex::new(HashMap::new())),
            vault_bucket,
            metrics: AuthMetrics::new(),
        }
    }

    /// Attempts to consume one auth token for the given identity.
    ///
    /// Validates `identity` before touching the bucket store.  Records
    /// attempt, success, and failure metrics via [`AuthMetrics`].
    ///
    /// # Errors
    ///
    /// - [`AuthError::Validation`] — `identity` failed key validation.
    /// - [`AuthError::RateLimited`] — the bucket for this identity is exhausted.
    pub fn check_auth_rate_limit(&self, identity: &str) -> Result<(), AuthError> {
        validate_identity_key(identity)?;

        self.metrics.record_attempt();

        let limiter = self.get_or_create_auth_bucket(identity);

        if limiter.try_acquire() {
            self.metrics.record_success();
            Ok(())
        } else {
            self.metrics.record_failure();
            let retry_after = limiter
                .time_until_available()
                .map(|d| d.as_secs())
                .unwrap_or(self.config.window.as_secs());
            tracing::warn!(
                identity = %identity,
                retry_after_secs = retry_after,
                "Auth rate limit exceeded"
            );
            Err(AuthError::RateLimited(retry_after))
        }
    }

    /// Attempts to consume one vault-probe token from the shared probe bucket.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::RateLimited`] when the vault probe bucket is
    /// exhausted.
    pub fn check_vault_probe_rate_limit(&self) -> Result<(), AuthError> {
        if self.vault_bucket.try_acquire() {
            Ok(())
        } else {
            let retry_after = self
                .vault_bucket
                .time_until_available()
                .map(|d| d.as_secs())
                .unwrap_or(self.config.window.as_secs());
            tracing::warn!(
                retry_after_secs = retry_after,
                "Vault probe rate limit exceeded"
            );
            Err(AuthError::RateLimited(retry_after))
        }
    }

    /// Returns the number of remaining auth tokens for `identity`.
    ///
    /// Returns `None` if `identity` fails validation or has no bucket yet.
    pub fn remaining_auth_tokens(&self, identity: &str) -> Option<u32> {
        validate_identity_key(identity).ok()?;
        let map = self.auth_buckets.lock().ok()?;
        map.get(identity).map(|l| l.available_tokens())
    }

    /// Returns the number of remaining vault probe tokens.
    pub fn remaining_vault_probe_tokens(&self) -> u32 {
        self.vault_bucket.available_tokens()
    }

    /// Returns a snapshot of the auth metrics.
    pub fn metrics(&self) -> &AuthMetrics {
        &self.metrics
    }

    /// Resets all per-identity auth buckets and the vault probe bucket.
    ///
    /// Intended for testing; in production prefer letting buckets refill
    /// naturally.
    pub fn reset_all(&self) {
        if let Ok(map) = self.auth_buckets.lock() {
            for limiter in map.values() {
                limiter.reset();
            }
        }
        self.vault_bucket.reset();
        self.metrics.reset();
    }

    // -- private helpers --

    fn get_or_create_auth_bucket(&self, identity: &str) -> RateLimiter {
        let mut map = self.auth_buckets.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(identity.to_string())
            .or_insert_with(|| {
                RateLimiter::with_config(RateLimitConfig {
                    max_requests: self.config.auth_limit,
                    window: self.config.window,
                    strategy: RateLimitStrategy::TokenBucket,
                })
            })
            .clone()
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AuthRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRateLimiter")
            .field("auth_limit", &self.config.auth_limit)
            .field("vault_probe_limit", &self.config.vault_probe_limit)
            .field("window", &self.config.window)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_identity_key --

    #[test]
    fn valid_key_accepted() {
        assert!(validate_identity_key("api-key-abc123").is_ok());
        assert!(validate_identity_key("ip:192.168.1.1").is_ok());
        assert!(validate_identity_key("tenant_uuid_here").is_ok());
    }

    #[test]
    fn empty_key_rejected() {
        assert!(validate_identity_key("").is_err());
    }

    #[test]
    fn key_too_long_rejected() {
        let key = "a".repeat(MAX_IDENTITY_KEY_LEN + 1);
        assert!(validate_identity_key(&key).is_err());
    }

    #[test]
    fn key_with_invalid_chars_rejected() {
        assert!(validate_identity_key("key with space").is_err());
        assert!(validate_identity_key("key@domain").is_err());
        assert!(validate_identity_key("key#hash").is_err());
    }

    #[test]
    fn key_at_max_length_accepted() {
        let key = "a".repeat(MAX_IDENTITY_KEY_LEN);
        assert!(validate_identity_key(&key).is_ok());
    }

    // -- check_auth_rate_limit --

    #[test]
    fn allows_requests_within_limit() {
        let config = AuthRateLimitConfig {
            auth_limit: 3,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        assert!(limiter.check_auth_rate_limit("user-abc").is_ok());
        assert!(limiter.check_auth_rate_limit("user-abc").is_ok());
        assert!(limiter.check_auth_rate_limit("user-abc").is_ok());
    }

    #[test]
    fn rejects_requests_over_limit() {
        let config = AuthRateLimitConfig {
            auth_limit: 2,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("user-xyz").ok();
        limiter.check_auth_rate_limit("user-xyz").ok();
        let result = limiter.check_auth_rate_limit("user-xyz");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::RateLimited(_)));
    }

    #[test]
    fn invalid_key_returns_validation_error() {
        let limiter = AuthRateLimiter::new();
        let result = limiter.check_auth_rate_limit("bad key!");
        assert!(matches!(result.unwrap_err(), AuthError::Validation(_)));
    }

    #[test]
    fn different_identities_have_independent_buckets() {
        let config = AuthRateLimitConfig {
            auth_limit: 1,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("user-a").ok();
        // user-a is exhausted; user-b should still be allowed.
        assert!(limiter.check_auth_rate_limit("user-b").is_ok());
    }

    #[test]
    fn metrics_record_attempts_and_failures() {
        let config = AuthRateLimitConfig {
            auth_limit: 1,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("user-m").ok();
        limiter.check_auth_rate_limit("user-m").ok(); // rejected
        assert_eq!(limiter.metrics().total_attempts(), 2);
        assert_eq!(limiter.metrics().successful_auths(), 1);
        assert_eq!(limiter.metrics().failed_auths(), 1);
    }

    #[test]
    fn metrics_record_validation_errors_separately() {
        let limiter = AuthRateLimiter::new();
        // Invalid key — validation error, not counted as attempt.
        limiter.check_auth_rate_limit("bad key!").ok();
        assert_eq!(limiter.metrics().total_attempts(), 0);
    }

    // -- check_vault_probe_rate_limit --

    #[test]
    fn vault_probe_allows_within_limit() {
        let config = AuthRateLimitConfig {
            auth_limit: 10,
            vault_probe_limit: 3,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        assert!(limiter.check_vault_probe_rate_limit().is_ok());
        assert!(limiter.check_vault_probe_rate_limit().is_ok());
        assert!(limiter.check_vault_probe_rate_limit().is_ok());
    }

    #[test]
    fn vault_probe_rejects_over_limit() {
        let config = AuthRateLimitConfig {
            auth_limit: 10,
            vault_probe_limit: 1,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_vault_probe_rate_limit().ok();
        let result = limiter.check_vault_probe_rate_limit();
        assert!(matches!(result.unwrap_err(), AuthError::RateLimited(_)));
    }

    // -- remaining tokens --

    #[test]
    fn remaining_tokens_decrements_on_acquire() {
        let config = AuthRateLimitConfig {
            auth_limit: 5,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("user-r").ok();
        assert_eq!(limiter.remaining_auth_tokens("user-r"), Some(4));
    }

    #[test]
    fn remaining_tokens_none_for_unknown_identity() {
        let limiter = AuthRateLimiter::new();
        assert_eq!(limiter.remaining_auth_tokens("never-seen"), None);
    }

    #[test]
    fn remaining_tokens_none_for_invalid_key() {
        let limiter = AuthRateLimiter::new();
        assert_eq!(limiter.remaining_auth_tokens("bad key!"), None);
    }

    // -- reset_all --

    #[test]
    fn reset_all_restores_full_buckets() {
        let config = AuthRateLimitConfig {
            auth_limit: 3,
            vault_probe_limit: 2,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("user-reset").ok();
        limiter.check_vault_probe_rate_limit().ok();
        limiter.reset_all();
        assert_eq!(limiter.remaining_auth_tokens("user-reset"), Some(3));
        assert_eq!(limiter.remaining_vault_probe_tokens(), 2);
    }

    // -- clone shares state --

    #[test]
    fn clone_shares_bucket_store() {
        let config = AuthRateLimitConfig {
            auth_limit: 4,
            vault_probe_limit: 5,
            window: Duration::from_secs(60),
        };
        let limiter = AuthRateLimiter::with_config(config);
        limiter.check_auth_rate_limit("shared-user").ok();
        let clone = limiter.clone();
        // Clone should see the same bucket state.
        assert_eq!(clone.remaining_auth_tokens("shared-user"), Some(3));
    }
}
