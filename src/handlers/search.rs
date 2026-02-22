use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

use crate::db::queries;
use crate::db::pool_manager::PoolManager;
use crate::schemas::TransactionSchema;
use crate::utils::cursor;

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub min_amount: Option<String>,
    pub max_amount: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub stellar_account: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub total: i64,
    pub results: Vec<TransactionSchema>,
    pub next_cursor: Option<String>,
}

pub async fn search_transactions(
    State(pool_manager): State<PoolManager>,
    Query(params): Query<SearchParams>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let limit = params.limit.unwrap_or(50).min(100).max(1);
    
    let from_date = if let Some(from_str) = &params.from {
        Some(
            DateTime::parse_from_rfc3339(from_str)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid 'from' date: {}", e)))?
                .with_timezone(&Utc),
        )
    } else {
        None
    };
    
    let to_date = if let Some(to_str) = &params.to {
        Some(
            DateTime::parse_from_rfc3339(to_str)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid 'to' date: {}", e)))?
                .with_timezone(&Utc),
        )
    } else {
        None
    };
    
    let min_amount = if let Some(min_str) = &params.min_amount {
        Some(
            min_str
                .parse::<BigDecimal>()
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid 'min_amount': {}", e)))?,
        )
    } else {
        None
    };
    
    let max_amount = if let Some(max_str) = &params.max_amount {
        Some(
            max_str
                .parse::<BigDecimal>()
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid 'max_amount': {}", e)))?,
        )
    } else {
        None
    };
    
    let cursor_data = if let Some(cursor_str) = &params.cursor {
        Some(
            cursor::decode(cursor_str)
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid cursor: {}", e)))?,
        )
    } else {
        None
    };
    
    let pool = pool_manager.replica().unwrap_or(pool_manager.primary());
    
    let (total, transactions) = queries::search_transactions(
        pool,
        params.status.as_deref(),
        params.asset_code.as_deref(),
        min_amount.as_ref(),
        max_amount.as_ref(),
        from_date,
        to_date,
        params.stellar_account.as_deref(),
        limit,
        cursor_data,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)))?;
    
    let results: Vec<TransactionSchema> = transactions
        .iter()
        .map(|tx| TransactionSchema {
            id: tx.id.to_string(),
            stellar_account: tx.stellar_account.clone(),
            amount: tx.amount.to_string(),
            asset_code: tx.asset_code.clone(),
            status: tx.status.clone(),
            created_at: tx.created_at,
            updated_at: tx.updated_at,
            anchor_transaction_id: tx.anchor_transaction_id.clone(),
            callback_type: tx.callback_type.clone(),
            callback_status: tx.callback_status.clone(),
            settlement_id: tx.settlement_id.map(|id| id.to_string()),
        })
        .collect();
    
    let next_cursor = if transactions.len() == limit as usize {
        transactions.last().map(|tx| cursor::encode(tx.created_at, tx.id))
    } else {
        None
    };
    
    Ok(Json(SearchResponse {
        total,
        results,
        next_cursor,
    }))
}
