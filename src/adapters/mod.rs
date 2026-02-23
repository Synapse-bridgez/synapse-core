//! Adapters: concrete implementations of ports.
//! These connect the application to external systems (DB, APIs, etc.).

pub mod postgres_transaction_repository;

pub use postgres_transaction_repository::PostgresTransactionRepository;
