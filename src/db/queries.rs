
pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    sqlx::query_as!(
        Transaction,
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING id, stellar_account, amount, asset_code, status,
                  created_at, updated_at, anchor_transaction_id, callback_type, callback_status
        "#,
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

use sqlx::{PgPool, Result, Postgres, Transaction as SqlxTransaction};
use crate::db::models::{Transaction, Settlement, TransactionDlq};
use crate::db::audit::{AuditLog, ENTITY_TRANSACTION, ENTITY_SETTLEMENT};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde_json::json;

// --- Transaction Queries ---

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    let mut transaction = pool.begin().await?;
    
    let result = sqlx::query_as::<_, Transaction>(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status, settlement_id
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING *
        "#
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
    .bind(tx.settlement_id)
    .fetch_one(pool)
    .await?;

    Ok(Transaction {
        id: row.get("id"),
        stellar_account: row.get("stellar_account"),
        amount: row.get("amount"),
        asset_code: row.get("asset_code"),
        status: row.get("status"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        anchor_transaction_id: row.get("anchor_transaction_id"),
        callback_type: row.get("callback_type"),
        callback_status: row.get("callback_status"),
    })
}

pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction> {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

pub async fn list_transactions(
    pool: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    backward: bool,
) -> Result<Vec<Transaction>> {
    // We implement cursor-based pagination on (created_at, id).
    // Default ordering for the API is newest-first (created_at DESC, id DESC).
    // For forward pagination (older items) we query WHERE (created_at, id) < (cursor)
    // For backward pagination (newer items) we query WHERE (created_at, id) > (cursor)

    if let Some((ts, id)) = cursor {
        if !backward {
            // forward page: older records than cursor
            let q = sqlx::query_as::<_, Transaction>(
                "SELECT * FROM transactions WHERE (created_at, id) < ($1, $2) ORDER BY created_at DESC, id DESC LIMIT $3",
            )
            .bind(ts)
            .bind(id)
            .bind(limit)
            .fetch_all(pool)
            .await?;
            Ok(q)
        } else {
            // backward page: newer records than cursor; fetch asc then reverse to keep newest-first
            let mut rows = sqlx::query_as::<_, Transaction>(
                "SELECT * FROM transactions WHERE (created_at, id) > ($1, $2) ORDER BY created_at ASC, id ASC LIMIT $3",
            )
            .bind(ts)
            .bind(id)
            .bind(limit)
            .fetch_all(pool)
            .await?;
            rows.reverse();
            Ok(rows)
        }
    } else {
        if !backward {
            // first page, newest first
            let q = sqlx::query_as::<_, Transaction>(
                "SELECT * FROM transactions ORDER BY created_at DESC, id DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(pool)
            .await?;
            Ok(q)
        } else {
            // backward without cursor -> return last page (oldest first reversed)
            let mut rows = sqlx::query_as::<_, Transaction>(
                "SELECT * FROM transactions ORDER BY created_at ASC, id ASC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(pool)
            .await?;
            rows.reverse();
            Ok(rows)
        }
    }
}

pub async fn get_unsettled_transactions(
    executor: &mut SqlxTransaction<'_, Postgres>,
    asset_code: &str,
    end_time: DateTime<Utc>,
) -> Result<Vec<Transaction>> {
    sqlx::query_as::<_, Transaction>(
        r#"
        SELECT * FROM transactions
        WHERE status = 'completed'
        AND settlement_id IS NULL
        AND asset_code = $1
        AND updated_at <= $2
        FOR UPDATE
        "#
    )
    .bind(asset_code)
    .bind(end_time)
    .fetch_all(&mut **executor)
    .await
}

pub async fn update_transactions_settlement(
    executor: &mut SqlxTransaction<'_, Postgres>,
    tx_ids: &[Uuid],
    settlement_id: Uuid,
) -> Result<()> {
    sqlx::query(
        "UPDATE transactions SET settlement_id = $1, updated_at = NOW() WHERE id = ANY($2)"
    )
    .bind(settlement_id)
    .bind(tx_ids)
    .execute(&mut **executor)
    .await?;
    
    // Audit log: record settlement_id update for each transaction
    for tx_id in tx_ids {
        AuditLog::log_field_update(
            executor,
            *tx_id,
            ENTITY_TRANSACTION,
            "settlement_id",
            json!(null),
            json!(settlement_id.to_string()),
            "system",
        )
        .await?;
    }
    
    Ok(())
}

// --- Settlement Queries ---

pub async fn insert_settlement(
    executor: &mut SqlxTransaction<'_, Postgres>,
    settlement: &Settlement,
) -> Result<Settlement> {
    let result = sqlx::query_as::<_, Settlement>(
        r#"
        INSERT INTO settlements (
            id, asset_code, total_amount, tx_count, period_start, period_end, status, created_at, updated_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING *
        "#
    )
    .bind(settlement.id)
    .bind(&settlement.asset_code)
    .bind(&settlement.total_amount)
    .bind(settlement.tx_count)
    .bind(settlement.period_start)
    .bind(settlement.period_end)
    .bind(&settlement.status)
    .bind(settlement.created_at)
    .bind(settlement.updated_at)
    .fetch_one(&mut **executor)
    .await

pub async fn get_transaction(pool: &PgPool, id: i32) -> Result<Transaction> {
    sqlx::query_as!(Transaction, "SELECT * FROM transactions WHERE id = $1", id)
        .fetch_one(pool)
        .await
}

pub async fn get_settlement(pool: &PgPool, id: Uuid) -> Result<Settlement> {
    sqlx::query_as::<_, Settlement>("SELECT * FROM settlements WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

pub async fn list_settlements(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Settlement>> {
    sqlx::query_as::<_, Settlement>("SELECT * FROM settlements ORDER BY created_at DESC LIMIT $1 OFFSET $2")
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
}

pub async fn get_unique_assets_to_settle(pool: &PgPool) -> Result<Vec<String>> {
    let rows = sqlx::query(
        "SELECT DISTINCT asset_code FROM transactions WHERE status = 'completed' AND settlement_id IS NULL"
    )
    .fetch_all(pool)
    .await?;
    
    Ok(rows.into_iter().map(|r| {
        use sqlx::Row;
        r.get:: <String, _>("asset_code")
    }).collect())
}
