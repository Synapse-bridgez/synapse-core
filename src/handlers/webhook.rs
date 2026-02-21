use crate::use_cases::DepositInput;
use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub id: String,
    pub anchor_transaction_id: String,
}

#[derive(Debug, Serialize)]
pub struct WebhookResponse {
    pub success: bool,
    pub message: String,
}

/// Handle incoming webhook callbacks.
/// The idempotency middleware should be applied to this handler.
pub async fn handle_webhook(
    State(state): State<AppState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    tracing::info!("Processing webhook with id: {}", payload.id);

    let input = DepositInput {
        id: payload.id,
        anchor_transaction_id: payload.anchor_transaction_id,
        stellar_account: None,
        amount: None,
        asset_code: None,
    };

    match state.process_deposit.execute(input).await {
        Ok(output) => (
            StatusCode::OK,
            Json(WebhookResponse {
                success: output.success,
                message: output.message,
            }),
        ),
        Err(e) => {
            tracing::error!("Webhook processing failed: {}", e);
            let (status, msg) = match e {
                crate::ports::RepositoryError::Database(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
                }
                crate::ports::RepositoryError::NotFound(_) => (StatusCode::NOT_FOUND, "Not found"),
            };
            (
                status,
                Json(WebhookResponse {
                    success: false,
                    message: msg.to_string(),
                }),
            )
        }
    }
}
