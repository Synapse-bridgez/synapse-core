use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub id: String,
    pub anchor_transaction_id: String,
    // Add other webhook fields as needed
}

#[derive(Debug, Serialize)]
pub struct WebhookResponse {
    pub success: bool,
    pub message: String,
}

/// Handle incoming webhook callbacks
/// The idempotency middleware should be applied to this handler
pub async fn handle_webhook(
    State(state): State<AppState>,
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
