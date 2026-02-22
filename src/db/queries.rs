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
    .fetch_one(&mut *transaction)
    .await?;

    // Audit log: transaction created
    AuditLog::log_creation(
        &mut transaction,
        result.id,
        ENTITY_TRANSACTION,
        json!({
            "stellar_account": result.stellar_account,
            "amount": result.amount.to_string(),
            "asset_code": result.asset_code,
            "status": result.status,
            "anchor_transaction_id": result.anchor_transaction_id,
            "callback_type": result.callback_type,
            "callback_status": result.callback_status,
        }),
        "system",
    )
    .await?;

    transaction.commit().await?;
    Ok(result)
}

pub async fn get_transaction(pool: &PgPool, id: Uuid) -> Result<Transaction> {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

pub async fn list_transactions(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Transaction>> {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions ORDER BY created_at DESC LIMIT $1 OFFSET $2")
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
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
    .await?;

    // Audit log: settlement created
    AuditLog::log_creation(
        executor,
        result.id,
        ENTITY_SETTLEMENT,
        json!({
            "asset_code": result.asset_code,
            "total_amount": result.total_amount.to_string(),
            "tx_count": result.tx_count,
            "period_start": result.period_start.to_rfc3339(),
            "period_end": result.period_end.to_rfc3339(),
            "status": result.status,
        }),
        "system",
    )
    .await?;

    Ok(result)
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

// --- Audit Log Queries ---

pub async fn get_audit_logs(
    pool: &PgPool,
    entity_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<(Uuid, String, String, String, Option<String>, Option<String>, String)>> {
    sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, Option<String>, String)>(
        r#"
        SELECT id, entity_id, entity_type, action, 
               old_val::text, new_val::text, actor
        FROM audit_logs
        WHERE entity_id = $1
        ORDER BY timestamp DESC
        LIMIT $2 OFFSET $3
        "#
    )
    .bind(entity_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await
}

pub async fn get_queue_status(pool: &PgPool) -> Result<std::collections::HashMap<String, i64>> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM transactions GROUP BY status"
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().collect())
}

pub async fn get_failed_transactions(pool: &PgPool) -> Result<Vec<TransactionDlq>> {
    sqlx::query_as::<_, TransactionDlq>(
        "SELECT * FROM transaction_dlq ORDER BY moved_to_dlq_at DESC LIMIT 50"
    )
    .fetch_all(pool)
    .await
}
