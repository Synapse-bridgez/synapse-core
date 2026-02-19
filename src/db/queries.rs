use sqlx::{PgPool, Result};
use uuid::Uuid;
use crate::db::models::Transaction;

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    sqlx::query_as::<_, Transaction>(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, anchor_transaction_id, callback_type, callback_status) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *"
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
    .fetch_one(pool)
    .await
}

pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction> {
    sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE id = $1"
    )
    .bind(id)
    .fetch_one(pool)
    .await
}

pub async fn list_transactions(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Transaction>> {
    sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions ORDER BY created_at DESC LIMIT $1 OFFSET $2"
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}