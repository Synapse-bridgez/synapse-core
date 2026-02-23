//! Port (trait) for transaction persistence.
//! Implementations can be Postgres, in-memory (for tests), etc.

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::Transaction;

/// Result type for repository operations.
pub type RepositoryResult<T> = Result<T, RepositoryError>;

/// Repository errors.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Port for persisting and querying transactions.
#[async_trait]
pub trait TransactionRepository: Send + Sync {
    /// Insert a new transaction.
    async fn insert(&self, tx: &Transaction) -> RepositoryResult<Transaction>;

    /// Get a transaction by ID.
    async fn get_by_id(&self, id: Uuid) -> RepositoryResult<Transaction>;

    /// List transactions with pagination.
    async fn list(&self, limit: i64, offset: i64) -> RepositoryResult<Vec<Transaction>>;
}
