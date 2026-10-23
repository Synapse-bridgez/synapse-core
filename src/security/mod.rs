/// Security module — rate limiting, session validation, and connection pooling.
pub mod connection_pool;
pub mod error;
pub mod session;

pub use connection_pool::{SecurityConnectionPool, SecurityPoolConfig, SecurityPoolError};
pub use error::SecurityError;
pub use session::*;
