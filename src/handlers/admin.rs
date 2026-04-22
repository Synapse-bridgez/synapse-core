use axum::{http::StatusCode, response::Json, extract::State};
use serde_json::json;
use crate::AppState;
use sqlx::FromRow;
use chrono::{DateTime, Utc};

#[derive(FromRow)]
struct WebhookEndpoint {
    id: uuid::Uuid,
    url: String,
    circuit_state: String,
    circuit_failure_count: i32,
    circuit_opened_at: Option<DateTime<Utc>>,
}

pub async fn get_circuit_breakers(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // TODO: Add admin authentication check

    let now = Utc::now();

    // Horizon breaker
    let horizon_state = state.horizon_breaker.get_state().await;
    let horizon_time_in_state = if let Some(opened_at) = horizon_state.opened_at {
        now.signed_duration_since(opened_at).num_seconds()
    } else {
        0
    };

    // Redis breaker
    let redis_state = state.redis_breaker.get_state().await;
    let redis_time_in_state = if let Some(opened_at) = redis_state.opened_at {
        now.signed_duration_since(opened_at).num_seconds()
    } else {
        0
    };

    // Postgres breaker
    let postgres_state = state.postgres_breaker.get_state().await;
    let postgres_time_in_state = if let Some(opened_at) = postgres_state.opened_at {
        now.signed_duration_since(opened_at).num_seconds()
    } else {
        0
    };

    // Webhook endpoints
    let webhook_endpoints: Vec<WebhookEndpoint> = sqlx::query_as(
        "SELECT id, url, circuit_state, circuit_failure_count, circuit_opened_at FROM webhook_endpoints"
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default(); // If table doesn't exist, empty list

    let webhook_states: Vec<serde_json::Value> = webhook_endpoints.into_iter().map(|ep| {
        let time_in_state = if let Some(opened_at) = ep.circuit_opened_at {
            now.signed_duration_since(opened_at).num_seconds()
        } else {
            0
        };
        json!({
            "id": ep.id,
            "url": ep.url,
            "state": ep.circuit_state,
            "failure_count": ep.circuit_failure_count,
            "time_in_current_state": time_in_state
        })
    }).collect();

    let response = json!({
        "horizon": {
            "state": horizon_state.state,
            "failure_count": horizon_state.failure_count,
            "time_in_current_state": horizon_time_in_state,
            "last_error": horizon_state.last_error
        },
        "redis": {
            "state": redis_state.state,
            "failure_count": redis_state.failure_count,
            "time_in_current_state": redis_time_in_state,
            "last_error": redis_state.last_error
        },
        "postgres": {
            "state": postgres_state.state,
            "failure_count": postgres_state.failure_count,
            "time_in_current_state": postgres_time_in_state,
            "last_error": postgres_state.last_error
        },
        "webhook_endpoints": webhook_states
    });

    Ok(Json(response))
}