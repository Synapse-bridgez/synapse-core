//! Telemetry module with input validation, reconnection logic, and connection pooling.

pub mod connection_pool;
pub mod input_validation;
pub mod reconnection;

pub use connection_pool::{ConnectionPool, PoolConfig, PoolError};
pub use input_validation::InputValidator;
pub use reconnection::ReconnectionManager;
