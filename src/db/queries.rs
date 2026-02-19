use sqlx::{PgPool, Result};
use crate::db::models::Transaction;

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    sqlx::query_as!(
        Transaction,
        "INSERT INTO transactions (id, amount, created_at) VALUES ($1, $2, $3) RETURNING *",
        tx.id,
        tx.amount,
        tx.created_at
    )
    .fetch_one(pool)
    .await
}

pub async fn get_transaction(pool: &PgPool, id: i32) -> Result<Transaction> {
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