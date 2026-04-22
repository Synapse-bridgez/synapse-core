use axum::{extract::Path, http::StatusCode, response::Json, extract::State};
use serde_json::json;
use crate::AppState;
use uuid::Uuid;

pub async fn reset_webhook_circuit(
    Path(endpoint_id): Path<Uuid>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // TODO: Add admin authentication check

    let result = sqlx::query(
        "UPDATE webhook_endpoints SET circuit_state = 'closed', circuit_failure_count = 0, circuit_opened_at = NULL WHERE id = $1"
    )
    .bind(endpoint_id)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => Ok(Json(json!({ "message": "Circuit breaker reset successfully" }))),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}