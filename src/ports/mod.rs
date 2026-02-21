//! Ports: trait definitions (interfaces) for external dependencies.
//! The application defines these; adapters implement them.

pub mod transaction_repository;

pub use transaction_repository::{RepositoryError, RepositoryResult, TransactionRepository};
