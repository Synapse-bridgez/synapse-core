use sqlx::{PgPool, Result, Postgres, Transaction as SqlxTransaction, Row};
use crate::db::models::{Transaction, Settlement, TransactionDlq};
use crate::db::audit::{AuditLog, ENTITY_TRANSACTION, ENTITY_SETTLEMENT};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::types::BigDecimal;

// --- Transaction Queries ---

pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    let result = sqlx::query_as::<_, Transaction>(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
            settlement_id, memo, memo_type, metadata
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
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
    .bind(&tx.memo)
    .bind(&tx.memo_type)
    .bind(&tx.metadata)
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
            "memo": result.memo,
            "memo_type": result.memo_type,
            "metadata": result.metadata,
        }),
        "system",
    )
    .await?;

    Ok(result)
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
        r.get::<String, _>("asset_code")
    }).collect())
}

// --- Transaction Search ---

pub async fn search_transactions(
    pool: &PgPool,
    status: Option<&str>,
    asset_code: Option<&str>,
    min_amount: Option<&BigDecimal>,
    max_amount: Option<&BigDecimal>,
    from_date: Option<DateTime<Utc>>,
    to_date: Option<DateTime<Utc>>,
    stellar_account: Option<&str>,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> Result<(i64, Vec<Transaction>)> {
    // Build dynamic WHERE clause
    let mut conditions = Vec::new();
    let mut param_count = 1;
    
    if status.is_some() {
        conditions.push(format!("status = ${}", param_count));
        param_count += 1;
    }
    
    if asset_code.is_some() {
        conditions.push(format!("asset_code = ${}", param_count));
        param_count += 1;
    }
    
    if min_amount.is_some() {
        conditions.push(format!("amount >= ${}", param_count));
        param_count += 1;
    }
    
    if max_amount.is_some() {
        conditions.push(format!("amount <= ${}", param_count));
        param_count += 1;
    }
    
    if from_date.is_some() {
        conditions.push(format!("created_at >= ${}", param_count));
        param_count += 1;
    }
    
    if to_date.is_some() {
        conditions.push(format!("created_at <= ${}", param_count));
        param_count += 1;
    }
    
    if stellar_account.is_some() {
        conditions.push(format!("stellar_account = ${}", param_count));
        param_count += 1;
    }
    
    // Add cursor condition
    if cursor.is_some() {
        conditions.push(format!("(created_at, id) < (${}, ${})", param_count, param_count + 1));
        param_count += 2;
    }
    
    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };
    
    // Build count query
    let count_query = format!("SELECT COUNT(*) as count FROM transactions {}", where_clause);
    
    // Build data query with pagination
    let data_query = format!(
        "SELECT * FROM transactions {} ORDER BY created_at DESC, id DESC LIMIT ${}",
        where_clause, param_count
    );
    
    // Execute count query
    let mut count_query_builder = sqlx::query(&count_query);
    
    if let Some(s) = status {
        count_query_builder = count_query_builder.bind(s);
    }
    if let Some(a) = asset_code {
        count_query_builder = count_query_builder.bind(a);
    }
    if let Some(min) = min_amount {
        count_query_builder = count_query_builder.bind(min);
    }
    if let Some(max) = max_amount {
        count_query_builder = count_query_builder.bind(max);
    }
    if let Some(from) = from_date {
        count_query_builder = count_query_builder.bind(from);
    }
    if let Some(to) = to_date {
        count_query_builder = count_query_builder.bind(to);
    }
    if let Some(acc) = stellar_account {
        count_query_builder = count_query_builder.bind(acc);
    }
    if let Some((ts, id)) = cursor {
        count_query_builder = count_query_builder.bind(ts).bind(id);
    }
    
    let count_row = count_query_builder.fetch_one(pool).await?;
    let total: i64 = count_row.try_get("count")?;
    
    // Execute data query
    let mut data_query_builder = sqlx::query_as::<_, Transaction>(&data_query);
    
    if let Some(s) = status {
        data_query_builder = data_query_builder.bind(s);
    }
    if let Some(a) = asset_code {
        data_query_builder = data_query_builder.bind(a);
    }
    if let Some(min) = min_amount {
        data_query_builder = data_query_builder.bind(min);
    }
    if let Some(max) = max_amount {
        data_query_builder = data_query_builder.bind(max);
    }
    if let Some(from) = from_date {
        data_query_builder = data_query_builder.bind(from);
    }
    if let Some(to) = to_date {
        data_query_builder = data_query_builder.bind(to);
    }
    if let Some(acc) = stellar_account {
        data_query_builder = data_query_builder.bind(acc);
    }
    if let Some((ts, id)) = cursor {
        data_query_builder = data_query_builder.bind(ts).bind(id);
    }
    data_query_builder = data_query_builder.bind(limit);
    
    let transactions = data_query_builder.fetch_all(pool).await?;
    
    Ok((total, transactions))
}
