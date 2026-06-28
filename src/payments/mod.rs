/// Payments module — settlement logic, data export, and pagination.
pub mod connection_pool;
pub mod error;
pub mod export;
pub mod input_validation;
pub mod pagination;

pub use connection_pool::{PaymentsConnectionPool, PaymentsPoolConfig, PaymentsPoolError};
pub use error::PaymentError;
pub use export::*;
pub use pagination::{PaginatedResponse, PaginationConfig, PaginationManager, PaginationParams};
