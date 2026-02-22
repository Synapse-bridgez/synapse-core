use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::db::queries;
use crate::error::AppError;
use utoipa::ToSchema;

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
    State(_state): State<ApiState>,
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
pub async fn callback(State(_state): State<ApiState>) -> impl IntoResponse {
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
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let transaction = queries::get_transaction(&state.app_state.db, id).await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("Transaction {} not found", id)),
            _ => AppError::DatabaseError(e.to_string()),
        })?;

    Ok(Json(transaction))}
