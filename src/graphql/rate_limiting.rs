//! Secure rate limiting for the GraphQL layer (async-graphql schema extension).
//!
//! Provides per-identity token-bucket rate limiting enforced as an
//! [`async_graphql::Extension`] so every GraphQL request passes through the
//! check before any resolver runs.
//!
//! # Identity resolution
//!
//! The rate-limit key is derived from request context in priority order:
//! 1. `X-API-Key` header value (validated before use)
//! 2. `X-Tenant-ID` header value (prefixed `tenant:`)
//! 3. Literal `"anon"` for unauthenticated callers
//!
//! # Limits
//!
//! | Caller type | Default limit | Window |
//! |-------------|--------------|--------|
//! | Authenticated (API key / tenant) | 200 req | 60 s |
//! | Anonymous | 20 req | 60 s |
//!
//! Both limits are configurable via [`GraphQlRateLimitConfig`].
//!
//! # Security
//!
//! - API key values are validated (length + character allowlist) before being
//!   used as map keys to prevent memory exhaustion via crafted headers.
//! - Anonymous callers share a single bucket so a single unauthenticated
//!   client cannot exhaust per-identity state.
//! - The extension returns a structured [`async_graphql::ServerError`] with
//!   a stable `extensions.code` field (`RATE_LIMITED`) so clients can
//!   distinguish rate-limit errors from other errors programmatically.
//!
//! # Usage
//!
//! ```rust,ignore
//! use synapse_core::graphql::rate_limiting::{GraphQlRateLimiter, GraphQlRateLimitConfig};
//!
//! let schema = async_graphql::Schema::build(Query, Mutation, Subscription)
//!     .extension(GraphQlRateLimiter::new(GraphQlRateLimitConfig::default()))
//!     .finish();
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_graphql::{
    extensions::{Extension, ExtensionContext, ExtensionFactory, NextExecute},
    Response,
};

use crate::cache::rate_limiting::{RateLimitConfig, RateLimitStrategy, RateLimiter};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default request limit per window for authenticated callers.
const DEFAULT_AUTHED_LIMIT: u32 = 200;

/// Default request limit per window for anonymous callers.
const DEFAULT_ANON_LIMIT: u32 = 20;

/// Default rate-limit window.
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// Maximum allowed length for an API key used as a rate-limit key.
/// Mirrors the constraint in `src/auth/input_validation.rs`.
const MAX_API_KEY_LEN: usize = 256;

/// Minimum allowed length for an API key used as a rate-limit key.
const MIN_API_KEY_LEN: usize = 32;

/// Stable GraphQL error code returned when a caller is rate-limited.
pub const RATE_LIMITED_CODE: &str = "RATE_LIMITED";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the GraphQL rate-limit extension.
#[derive(Debug, Clone)]
pub struct GraphQlRateLimitConfig {
    /// Token limit per window for authenticated callers (API key / tenant).
    pub authed_limit: u32,
    /// Token limit per window for anonymous callers.
    pub anon_limit: u32,
    /// Duration of the rate-limit window.
    pub window: Duration,
}

impl Default for GraphQlRateLimitConfig {
    fn default() -> Self {
        Self {
            authed_limit: DEFAULT_AUTHED_LIMIT,
            anon_limit: DEFAULT_ANON_LIMIT,
            window: DEFAULT_WINDOW,
        }
    }
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validates an API key string for use as a rate-limit bucket key.
///
/// Accepts only ASCII alphanumeric characters plus `-`, `_`, and `.` to
/// prevent injection into key namespaces and to bound key length.
///
/// # Errors
///
/// Returns `Err(&'static str)` with a human-readable reason when the key
/// fails validation.
pub fn validate_rate_limit_key(key: &str) -> Result<(), &'static str> {
    if key.is_empty() {
        return Err("rate-limit key cannot be empty");
    }
    if key.len() < MIN_API_KEY_LEN {
        return Err("rate-limit key is too short");
    }
    if key.len() > MAX_API_KEY_LEN {
        return Err("rate-limit key exceeds maximum length");
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("rate-limit key contains invalid characters");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-identity bucket store
// ---------------------------------------------------------------------------

/// Thread-safe map from identity key → `RateLimiter`.
///
/// Wrapped in `Arc` so the factory and the extension instance share state.
#[derive(Clone, Default)]
struct BucketStore {
    inner: Arc<Mutex<HashMap<String, RateLimiter>>>,
}

impl BucketStore {
    /// Returns the limiter for `key`, creating it with `config` if absent.
    fn get_or_create(&self, key: &str, config: &RateLimitConfig) -> RateLimiter {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(key.to_string())
            .or_insert_with(|| RateLimiter::with_config(config.clone()))
            .clone()
    }
}

// ---------------------------------------------------------------------------
// ExtensionFactory
// ---------------------------------------------------------------------------

/// Factory registered on the schema that creates one extension instance per
/// request.  All instances share the same [`BucketStore`] so token counts
/// persist across requests.
pub struct GraphQlRateLimiter {
    config: GraphQlRateLimitConfig,
    store: BucketStore,
}

impl GraphQlRateLimiter {
    /// Creates a new rate-limiter factory with the given configuration.
    pub fn new(config: GraphQlRateLimitConfig) -> Self {
        Self {
            config,
            store: BucketStore::default(),
        }
    }
}

impl Default for GraphQlRateLimiter {
    fn default() -> Self {
        Self::new(GraphQlRateLimitConfig::default())
    }
}

impl ExtensionFactory for GraphQlRateLimiter {
    fn create(&self) -> Arc<dyn Extension> {
        Arc::new(GraphQlRateLimitExtension {
            config: self.config.clone(),
            store: self.store.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Extension (per-request)
// ---------------------------------------------------------------------------

struct GraphQlRateLimitExtension {
    config: GraphQlRateLimitConfig,
    store: BucketStore,
}

#[async_graphql::async_trait::async_trait]
impl Extension for GraphQlRateLimitExtension {
    async fn execute(
        &self,
        ctx: &ExtensionContext<'_>,
        operation_name: Option<&str>,
        next: NextExecute<'_>,
    ) -> Response {
        let (identity, is_authed) = resolve_identity(ctx);

        let limit_config = if is_authed {
            RateLimitConfig {
                max_requests: self.config.authed_limit,
                window: self.config.window,
                strategy: RateLimitStrategy::TokenBucket,
            }
        } else {
            RateLimitConfig {
                max_requests: self.config.anon_limit,
                window: self.config.window,
                strategy: RateLimitStrategy::TokenBucket,
            }
        };

        let limiter = self.store.get_or_create(&identity, &limit_config);

        if !limiter.try_acquire() {
            let remaining_hint = limiter
                .time_until_available()
                .map(|d| d.as_secs())
                .unwrap_or(self.config.window.as_secs());

            tracing::warn!(
                identity = %identity,
                operation = ?operation_name,
                retry_after_secs = remaining_hint,
                "GraphQL request rate-limited"
            );

            let err = async_graphql::Error::new("Too many requests — rate limit exceeded")
                .extend_with(|_, e| {
                    e.set("code", RATE_LIMITED_CODE);
                    e.set("retryAfter", remaining_hint);
                });

            return Response::from_errors(vec![err.into()]);
        }

        next.run(ctx, operation_name).await
    }
}

// ---------------------------------------------------------------------------
// Identity resolution
// ---------------------------------------------------------------------------

/// Extracts the rate-limit identity from the GraphQL extension context.
///
/// Returns `(key, is_authenticated)`.  The key is safe to use as a map key
/// — API key values are validated before being returned.
fn resolve_identity(ctx: &ExtensionContext<'_>) -> (String, bool) {
    // async-graphql stores HTTP header data via `ctx.data::<T>()`.
    // Headers are injected by the axum handler as `HeaderMap`.
    if let Ok(headers) = ctx.data::<axum::http::HeaderMap>() {
        // 1. X-API-Key
        if let Some(raw) = headers
            .get("x-api-key")
            .or_else(|| headers.get("X-API-Key"))
            .and_then(|v| v.to_str().ok())
        {
            if validate_rate_limit_key(raw).is_ok() {
                return (raw.to_string(), true);
            }
            // Key present but invalid — treat as anonymous to avoid leaking
            // information about the validation failure.
            tracing::debug!("GraphQL rate-limit: invalid API key format, falling back to anon");
        }

        // 2. X-Tenant-ID
        if let Some(tenant_id) = headers
            .get("X-Tenant-ID")
            .or_else(|| headers.get("x-tenant-id"))
            .and_then(|v| v.to_str().ok())
        {
            // Tenant IDs are UUIDs (36 chars) — safe to use directly.
            if !tenant_id.is_empty() && tenant_id.len() <= 64 {
                return (format!("tenant:{tenant_id}"), true);
            }
        }
    }

    ("anon".to_string(), false)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- validate_rate_limit_key --

    #[test]
    fn valid_key_accepted() {
        let key = "a".repeat(MIN_API_KEY_LEN);
        assert!(validate_rate_limit_key(&key).is_ok());
    }

    #[test]
    fn key_too_short_rejected() {
        let key = "a".repeat(MIN_API_KEY_LEN - 1);
        assert!(validate_rate_limit_key(&key).is_err());
    }

    #[test]
    fn key_too_long_rejected() {
        let key = "a".repeat(MAX_API_KEY_LEN + 1);
        assert!(validate_rate_limit_key(&key).is_err());
    }

    #[test]
    fn empty_key_rejected() {
        assert!(validate_rate_limit_key("").is_err());
    }

    #[test]
    fn key_with_invalid_chars_rejected() {
        let base = "a".repeat(MIN_API_KEY_LEN);
        assert!(validate_rate_limit_key(&format!("{base}@")).is_err());
        assert!(validate_rate_limit_key(&format!("{base} ")).is_err());
        assert!(validate_rate_limit_key(&format!("{base}#")).is_err());
    }

    #[test]
    fn key_with_allowed_special_chars_accepted() {
        let key = format!("{}-{}_{}", "a".repeat(10), "b".repeat(10), "c".repeat(12));
        assert!(validate_rate_limit_key(&key).is_ok());
    }

    // -- BucketStore --

    #[test]
    fn bucket_store_creates_limiter_on_first_access() {
        let store = BucketStore::default();
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = store.get_or_create("test-key", &config);
        assert_eq!(limiter.available_tokens(), 5);
    }

    #[test]
    fn bucket_store_reuses_existing_limiter() {
        let store = BucketStore::default();
        let config = RateLimitConfig {
            max_requests: 3,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let l1 = store.get_or_create("key", &config);
        l1.try_acquire();
        let l2 = store.get_or_create("key", &config);
        // Both share the same bucket — l2 should see 2 tokens remaining.
        assert_eq!(l2.available_tokens(), 2);
    }

    #[test]
    fn different_keys_have_independent_buckets() {
        let store = BucketStore::default();
        let config = RateLimitConfig {
            max_requests: 2,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let l1 = store.get_or_create("key-a", &config);
        l1.try_acquire();
        l1.try_acquire();
        let l2 = store.get_or_create("key-b", &config);
        // key-b bucket is untouched.
        assert_eq!(l2.available_tokens(), 2);
    }

    // -- GraphQlRateLimiter factory --

    #[test]
    fn factory_default_config() {
        let factory = GraphQlRateLimiter::default();
        assert_eq!(factory.config.authed_limit, DEFAULT_AUTHED_LIMIT);
        assert_eq!(factory.config.anon_limit, DEFAULT_ANON_LIMIT);
        assert_eq!(factory.config.window, DEFAULT_WINDOW);
    }

    #[test]
    fn factory_custom_config() {
        let cfg = GraphQlRateLimitConfig {
            authed_limit: 500,
            anon_limit: 10,
            window: Duration::from_secs(30),
        };
        let factory = GraphQlRateLimiter::new(cfg.clone());
        assert_eq!(factory.config.authed_limit, 500);
        assert_eq!(factory.config.anon_limit, 10);
    }
}
