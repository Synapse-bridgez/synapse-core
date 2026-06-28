//! Authentication module with input validation, metrics collection, health
//! checks, and rate limiting (vaultrs integration).

pub mod error;
pub mod health;
pub mod input_validation;
pub mod metrics;
pub mod rate_limiting;

pub use error::*;
pub use health::*;
pub use input_validation::*;
pub use metrics::*;
pub use rate_limiting::AuthRateLimiter;
