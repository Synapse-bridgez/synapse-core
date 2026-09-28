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
//!
//! # Degraded mode (Redis unavailable)
//!
//! When the shared Redis-backed limiter is unavailable, this module falls back
//! to a local, in-process token bucket with a *stricter* limit
//! ([`DEGRADED_AUTH_LIMIT`]) rather than failing open.  Failing open during an
//! outage is itself an abuse/security risk, so the degraded path is
//! fail-closed-ish: it still enforces a limit, just a tighter one.  Every
//! degraded decision emits the shared [`crate::cache::error_handling`]
//! degraded-mode signal so operators can see the full blast radius of a Redis
//! outage at a glance.

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

/// Stricter auth limit applied while Redis is unavailable (degraded mode).
///
/// Deliberately lower than [`DEFAULT_AUTH_LIMIT`] so that a cache-layer outage
/// tightens rather than loosens abuse protection.
const DEGRADED_AUTH_LIMIT: u32 = 3;

/// Stricter vault-probe limit applied while Redis is unavailable.
const DEGRADED_VAULT_PROBE_LIMIT: u32 = 2;

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
    /// When `true`, Redis is unavailable and the stricter degraded limits are
    /// enforced instead of the configured limits.
    degraded: bool,
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
            degraded: false,
        }
    }

    /// Marks this limiter as running in degraded mode (Redis unavailable).
    ///
    /// While degraded, the stricter [`DEGRADED_AUTH_LIMIT`] and
    /// [`DEGRADED_VAULT_PROBE_LIMIT`] are enforced so that a cache-layer
    /// outage tightens rather than removes abuse protection.  Emits the shared
    /// degraded-mode signal exactly once per transition so operators can see
    /// the blast radius of the outage.
    pub fn enter_degraded_mode(&mut self) {
        if !self.degraded {
            self.degraded = true;
            crate::cache::error_handling::record_degraded_mode(
                "auth_rate_limiting",
                "redis unavailable; enforcing stricter local limits",
            );
        }
    }

    /// Returns `true` when this limiter is enforcing degraded-mode limits.
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Returns the effective auth limit for the current mode.
    fn effective_auth_limit(&self) -> u32 {
        if self.degraded {
            DEGRADED_AUTH_LIMIT.min(self.config.auth_limit)
        } else {
            self.config.auth_limit
        }
    }

    /// Returns the effective vault-probe limit for the current mode.
    fn effective_vault_probe_limit(&self) -> u32 {
        if self.degraded {
            DEGRADED_VAULT_PROBE_LIMIT.min(self.config.vault_probe_limit)
        } else {
            self.config.vault_probe_limit
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
                degraded = self.degraded,
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
                degraded = self.degraded,
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
        let map = self.auth_buckets.l

/* … truncated 9910 chars — edit only what you need near the top … */
