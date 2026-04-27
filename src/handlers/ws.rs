use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, State,
    },
    http::HeaderMap,
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::AppState;

/// Maximum WebSocket connections allowed per client IP per minute.
const WS_RATE_LIMIT: u32 = 10;
const WS_RATE_LIMIT_WINDOW_SECS: i64 = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionStatusUpdate {
    pub transaction_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

/// Resolve the API token from query param or Authorization header.
fn extract_token(params: &WsQuery, headers: &HeaderMap) -> Option<String> {
    if let Some(t) = &params.token {
        if !t.is_empty() {
            return Some(t.clone());
        }
    }
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim_start_matches("Bearer ").to_string())
        .filter(|s| !s.is_empty())
}

/// Validate the token against tenant API keys and return the tenant_id on success.
async fn authenticate_token(pool: &sqlx::PgPool, token: &str) -> Option<Uuid> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT tenant_id FROM tenants WHERE api_key = $1 AND is_active = true",
    )
    .bind(token)
    .fetch_optional(pool)
    .await
    .ok()??;

    row.try_get::<Uuid, _>("tenant_id").ok()
}

/// Check and increment the per-IP connection rate limit.
/// Returns `true` if the connection is allowed.
async fn check_ip_rate_limit(redis_url: &str, client_ip: &str) -> bool {
    let client = match redis::Client::open(redis_url) {
        Ok(c) => c,
        Err(_) => return true, // fail open if Redis unavailable
    };
    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(_) => return true,
    };

    let key = format!("ws:ratelimit:{client_ip}");
    let count: u32 = conn.incr(&key, 1u32).await.unwrap_or(0);
    if count == 1 {
        let _: Result<(), _> = conn.expire(&key, WS_RATE_LIMIT_WINDOW_SECS).await;
    }
    count <= WS_RATE_LIMIT
}

/// WebSocket upgrade handler — requires a valid tenant API key.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let client_ip = addr.ip().to_string();

    // Rate-limit by client IP before doing anything else.
    if !check_ip_rate_limit(&state.redis_url, &client_ip).await {
        tracing::warn!(client_ip = %client_ip, "WebSocket connection rate limit exceeded");
        return axum::http::StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    // Require an auth token.
    let token = match extract_token(&params, &headers) {
        Some(t) => t,
        None => {
            tracing::warn!(client_ip = %client_ip, "WebSocket connection rejected: missing token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    };

    // Validate token against tenant API keys.
    let tenant_id = match authenticate_token(&state.db, &token).await {
        Some(id) => id,
        None => {
            tracing::warn!(
                client_ip = %client_ip,
                "WebSocket connection rejected: invalid token"
            );
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    };

    tracing::info!(
        tenant_id = %tenant_id,
        client_ip = %client_ip,
        "WebSocket connection authenticated"
    );

    ws.on_upgrade(move |socket| handle_socket(socket, state, tenant_id))
}

/// Handle an authenticated WebSocket connection, filtering events to the tenant's own updates.
async fn handle_socket(socket: WebSocket, state: AppState, tenant_id: Uuid) {
    let (mut sender, mut receiver) = socket.split();

    let mut rx = state.tx_broadcast.subscribe();

    // Receive task: handle incoming client messages.
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    tracing::debug!("Received text message: {}", text);
                }
                Message::Ping(_) => {
                    tracing::trace!("Received ping");
                }
                Message::Close(_) => {
                    tracing::info!(tenant_id = %tenant_id, "Client closed connection");
                    break;
                }
                _ => {}
            }
        }
    });

    // Send task: forward only this tenant's broadcast events + heartbeat.
    let mut send_task = tokio::spawn(async move {
        let mut heartbeat_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    if sender.send(Message::Ping(vec![])).await.is_err() {
                        tracing::info!(tenant_id = %tenant_id, "Client disconnected during heartbeat");
                        break;
                    }
                }
                result = rx.recv() => {
                    match result {
                        Ok(update) => {
                            // Only forward events belonging to this tenant.
                            if update.tenant_id != tenant_id {
                                continue;
                            }

                            let json = match serde_json::to_string(&update) {
                                Ok(j) => j,
                                Err(e) => {
                                    tracing::error!("Failed to serialize update: {}", e);
                                    continue;
                                }
                            };

                            if sender.send(Message::Text(json)).await.is_err() {
                                tracing::info!(tenant_id = %tenant_id, "Client disconnected");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(
                                tenant_id = %tenant_id,
                                "Client lagged behind by {} messages", n
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::info!("Broadcast channel closed");
                            break;
                        }
                    }
                }
            }
        }
    });

    tokio::select! {
        _ = (&mut send_task) => { recv_task.abort(); }
        _ = (&mut recv_task) => { send_task.abort(); }
    }

    tracing::info!(tenant_id = %tenant_id, "WebSocket connection closed");
}
