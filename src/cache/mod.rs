//! Caching module with rate limiting, input validation, and webhook security.

pub mod rate_limiting;
pub mod validation;
pub mod webhook;
//! Caching module with Redis-oriented input validation and rate limiting.
//!
//! - [`validation`] — key, value, TTL, and pattern checks before Redis I/O
//! - [`rate_limiting`] — in-process token bucket / sliding window limits
//!
//! Query result caching lives in [`crate::services::query_cache`] and calls
//! [`CacheValidator`] at get/set/invalidate boundaries. See
//! [cache input validation](../../../docs/cache-input-validation.md) for full details.

pub mod rate_limiting;
pub mod validation;

pub use rate_limiting::RateLimiter;
pub use validation::{CacheValidator, ValidationError, MAX_KEY_LENGTH, MAX_VALUE_SIZE};
