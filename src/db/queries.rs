use sqlx::{PgPool, Result};
use uuid::Uuid;
use crate::db::models::Transaction;

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    sqlx::query_as!(
        Transaction,
        r#"INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *"#,
        tx.id,
        tx.stellar_account,
        tx.amount,
        tx.asset_code,
        tx.status,
        tx.created_at,
        tx.updated_at,
        tx.anchor_transaction_id,
        tx.callback_type,
        tx.callback_status
    )
    .fetch_one(pool)
    .await
}

pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction> {
    sqlx::query_as!(
        Transaction,
        "SELECT * FROM transactions WHERE id = $1",
        id
    )
    .fetch_one(pool)
    .await
}

pub async fn list_transactions(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Transaction>> {
    sqlx::query_as!(
        Transaction,
        "SELECT * FROM transactions ORDER BY created_at DESC LIMIT $1 OFFSET $2",
        limit,
        offset
    )
    .fetch_all(pool)
    .await
}
