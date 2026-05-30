/// Authentication module with input validation, metrics, and error handling.
pub mod error;
pub mod input_validation;
pub mod metrics;

pub use error::*;
/// Authentication module with input validation, metrics collection, and health checks.
pub mod health;
pub mod input_validation;
pub mod metrics;

pub use health::*;
pub use input_validation::*;
pub use metrics::*;
