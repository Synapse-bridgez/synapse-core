//! Postgres implementation of TransactionRepository.

use async_trait::async_trait;
use sqlx::PgPool;
use uuid::Uuid;

use crate::domain::Transaction;
use crate::ports::{RepositoryError, RepositoryResult, TransactionRepository};

/// Postgres-backed transaction repository.
#[derive(Clone)]
pub struct PostgresTransactionRepository {
    pool: PgPool,
}

impl PostgresTransactionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TransactionRepository for PostgresTransactionRepository {
    async fn insert(&self, tx: &Transaction) -> RepositoryResult<Transaction> {
        let row = sqlx::query_as::<_, TransactionRow>(
            r#"
            INSERT INTO transactions (
                id, stellar_account, amount, asset_code, status,
                created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
                memo, memo_type, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING id, stellar_account, amount, asset_code, status,
                created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
                memo, memo_type, metadata
            "#,
        )
        .bind(tx.id)
        .bind(&tx.stellar_account)
        .bind(&tx.amount)
        .bind(&tx.asset_code)
        .bind(&tx.status)
        .bind(tx.created_at)
        .bind(tx.updated_at)
        .bind(&tx.anchor_transaction_id)
        .bind(&tx.callback_type)
        .bind(&tx.callback_status)
        .bind(&tx.memo)
        .bind(&tx.memo_type)
        .bind(&tx.metadata)
        .fetch_one(&self.pool)
        .await
        .map_err(RepositoryError::from)?;

        Ok(row.into_domain())
    }

    async fn get_by_id(&self, id: Uuid) -> RepositoryResult<Transaction> {
        let row = sqlx::query_as::<_, TransactionRow>("SELECT * FROM transactions WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(RepositoryError::from)?;

        row.map(|r| r.into_domain())
            .ok_or_else(|| RepositoryError::NotFound(id.to_string()))
    }

    async fn list(&self, limit: i64, offset: i64) -> RepositoryResult<Vec<Transaction>> {
        let rows = sqlx::query_as::<_, TransactionRow>(
            "SELECT * FROM transactions ORDER BY created_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(RepositoryError::from)?;

        Ok(rows.into_iter().map(|r| r.into_domain()).collect())
    }
}

/// Internal row type for SQLx. Not exposed outside the adapter.
#[derive(Debug, sqlx::FromRow)]
struct TransactionRow {
    id: Uuid,
    stellar_account: String,
    amount: bigdecimal::BigDecimal,
    asset_code: String,
    status: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
    memo: Option<String>,
    memo_type: Option<String>,
    metadata: Option<serde_json::Value>,
}

impl TransactionRow {
    fn into_domain(self) -> Transaction {
        Transaction {
            id: self.id,
            stellar_account: self.stellar_account,
            amount: self.amount,
            asset_code: self.asset_code,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
            anchor_transaction_id: self.anchor_transaction_id,
            callback_type: self.callback_type,
            callback_status: self.callback_status,
            memo: self.memo,
            memo_type: self.memo_type,
            metadata: self.metadata,
        }
    }
}
