use crate::AppState;
use axum::{
    extract::{Path, State, Query},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::db::queries;
use crate::error::AppError;
use utoipa::ToSchema;
use crate::utils::cursor as cursor_util;
use crate::db::models::Transaction as TxModel;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WebhookPayload {
    /// Unique webhook identifier
    pub id: String,
    /// Associated anchor transaction ID
    pub anchor_transaction_id: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookResponse {
    /// Whether the webhook was processed successfully
    pub success: bool,
    /// Response message
    pub message: String,
}

/// Handle incoming webhook callbacks
/// 
/// The idempotency middleware is applied to this handler.
/// Returns success status if the webhook is processed.
#[utoipa::path(
    post,
    path = "/webhook",
    request_body = WebhookPayload,
    responses(
        (status = 200, description = "Webhook processed successfully", body = WebhookResponse),
        (status = 400, description = "Invalid payload"),
        (status = 500, description = "Processing error")
    ),
    tag = "Webhooks"
)]
pub async fn handle_webhook(
    State(_state): State<AppState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    tracing::info!("Processing webhook with id: {}", payload.id);

    // Process the webhook (e.g., create transaction, update database)
    // This is where your business logic goes
    
    let response = WebhookResponse {
        success: true,
        message: format!("Webhook {} processed successfully", payload.id),
    };

    (StatusCode::OK, Json(response))
}

/// Callback endpoint for transactions (placeholder)
pub async fn callback(State(_state): State<crate::AppState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

/// Get a specific transaction
/// 
/// Returns details for a specific transaction by ID
#[utoipa::path(
    get,
    path = "/transactions/{id}",
    params(
        ("id" = String, Path, description = "Transaction ID")
    ),
    responses(
        (status = 200, description = "Transaction found", body = crate::schemas::TransactionSchema),
        (status = 404, description = "Transaction not found"),
        (status = 500, description = "Database error")
    ),
    tag = "Transactions"
)]
pub async fn get_transaction(
    State(state): State<crate::AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let transaction = queries::get_transaction(&state.db, id).await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("Transaction {} not found", id)),
            _ => AppError::Database(e),
        })?;

    Ok(Json(transaction))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// direction: "forward" (older items) or "backward" (newer items)
    pub direction: Option<String>,
}

#[utoipa::path(
    get,
    path = "/transactions",
    params(
        ("cursor" = Option<String>, Query, description = "Cursor for pagination"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("direction" = Option<String>, Query, description = "forward or backward")
    ),
    responses(
        (status = 200, description = "List transactions with pagination metadata"),
        (status = 500, description = "Database error")
    ),
    tag = "Transactions"
)]
pub async fn list_transactions(
    State(state): State<AppState>,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(25).min(100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok((ts, id)) => Some((ts, id)),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    // fetch one extra to determine has_more
    let fetch_limit = limit + 1;
    let mut rows = queries::list_transactions(&state.db, fetch_limit, decoded_cursor, backward)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    // next cursor is the last item in the returned rows
    let next_cursor = rows.last().map(|r: &TxModel| cursor_util::encode(r.created_at, r.id));

    let resp = serde_json::json!({
        "data": rows,
        "meta": {
            "next_cursor": next_cursor,
            "has_more": has_more
        }
    });

    Ok(Json(resp))
}

/// Wrapper to accept the router's ApiState without forcing all handlers to change.
pub async fn list_transactions_api(
    State(state): State<crate::AppState>,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, AppError> {
    // forward to the AppState-based handler
    // call the inner logic directly to avoid extractor conflicts
    let limit = params.limit.unwrap_or(25).min(100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok((ts, id)) => Some((ts, id)),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    let fetch_limit = limit + 1;
    let mut rows = queries::list_transactions(&state.db, fetch_limit, decoded_cursor, backward)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let next_cursor = rows.last().map(|r: &TxModel| cursor_util::encode(r.created_at, r.id));

    let resp = serde_json::json!({
        "data": rows,
        "meta": {
            "next_cursor": next_cursor,
            "has_more": has_more
        }
    });

    Ok(Json(resp))
}
