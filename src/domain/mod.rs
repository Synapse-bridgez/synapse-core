//! Domain layer: core business entities.
//! No external dependencies (database, HTTP, etc.).

pub mod transaction;

pub use transaction::Transaction;
