//! Caching module with rate limiting, input validation, webhook security, and error handling.
//! Caching module with Redis-oriented input validation and rate limiting.
//!
//! - [`validation`]    — key, value, TTL, and pattern checks before Redis I/O
//! - [`rate_limiting`] — in-process token bucket / sliding window limits
//! - [`error_handling`] — structured error types for cache operations
//!
//! Query result caching lives in [`crate::services::query_cache`] and calls
//! [`CacheValidator`] at get/set/invalidate boundaries.

pub mod error_handling;
pub mod rate_limiting;
pub mod validation;
pub mod webhook;

pub use error_handling::{CacheError, CacheResult, convert_redis_error};
pub use rate_limiting::RateLimiter;
pub use validation::{CacheValidator, ValidationError, MAX_KEY_LENGTH, MAX_VALUE_SIZE};
