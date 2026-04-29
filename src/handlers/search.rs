use crate::db::pool_manager::PoolManager;
use crate::error::AppError;
use crate::utils::cursor as cursor_util;
use axum::{
    extract::{Query, State},
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::types::BigDecimal;
use std::str::FromStr;
use tracing::instrument;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub min_amount: Option<String>,
    pub max_amount: Option<String>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub stellar_account: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[instrument(name = "search.transactions", skip(pool_manager, params))]
pub async fn search_transactions(
    State(pool_manager): State<PoolManager>,
    Query(params): Query<SearchQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(25).min(100);

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok((ts, id)) => Some((ts, id)),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {e}"))),
        }
    } else {
        None
    };

    let min_amount =
        match params.min_amount {
            Some(value) => Some(BigDecimal::from_str(&value).map_err(|_| {
                AppError::BadRequest("min_amount must be a valid decimal".to_string())
            })?),
            None => None,
        };

    let max_amount =
        match params.max_amount {
            Some(value) => Some(BigDecimal::from_str(&value).map_err(|_| {
                AppError::BadRequest("max_amount must be a valid decimal".to_string())
            })?),
            None => None,
        };

    let from_date = match params.from_date {
        Some(value) => Some(
            DateTime::parse_from_rfc3339(&value)
                .map_err(|_| AppError::BadRequest("from_date must be RFC 3339 format".to_string()))?
                .with_timezone(&Utc),
        ),
        None => None,
    };

    let to_date = match params.to_date {
        Some(value) => Some(
            DateTime::parse_from_rfc3339(&value)
                .map_err(|_| AppError::BadRequest("to_date must be RFC 3339 format".to_string()))?
                .with_timezone(&Utc),
        ),
        None => None,
    };

    let (pool, replica_used) = pool_manager.read_pool().await;
    let (total, transactions) = crate::db::queries::search_transactions(
        pool,
        params.status.as_deref(),
        params.asset_code.as_deref(),
        min_amount.as_ref(),
        max_amount.as_ref(),
        from_date,
        to_date,
        params.stellar_account.as_deref(),
        limit,
        decoded_cursor,
    )
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let resp = serde_json::json!({
        "total": total,
        "data": transactions,
    });

    let mut response = (StatusCode::OK, Json(resp)).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }

    Ok(response)
}

/// Wrapper for use with ApiState in create_app
pub async fn search_transactions_wrapper(
    State(api_state): State<crate::ApiState>,
    Query(params): Query<SearchQuery>,
) -> impl IntoResponse {
    search_transactions(State(api_state.app_state.pool_manager), Query(params)).await
}
