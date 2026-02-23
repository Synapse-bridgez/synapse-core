use axum::{
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionStatusUpdate {
    pub transaction_id: Uuid,
    pub status: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

/// WebSocket upgrade handler
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Validate token if provided
    if let Some(token) = params.token {
        if !validate_token(&token) {
            tracing::warn!("Invalid WebSocket authentication token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    }

    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// Handle individual WebSocket connection
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to broadcast channel
    let mut rx = state.tx_broadcast.subscribe();

    // Spawn task to handle incoming messages from client
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    tracing::debug!("Received text message: {}", text);
                    // Handle subscription filters or other client messages
                }
                Message::Ping(_) => {
                    tracing::trace!("Received ping");
                    // Axum handles pong automatically
                }
                Message::Close(_) => {
                    tracing::info!("Client closed connection");
                    break;
                }
                _ => {}
            }
        }
    });

    // Spawn task to send broadcast messages and heartbeats to client
    let mut send_task = tokio::spawn(async move {
        let mut heartbeat_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

        loop {
            tokio::select! {
                // Send heartbeat ping
                _ = heartbeat_interval.tick() => {
                    if sender.send(Message::Ping(vec![])).await.is_err() {
                        tracing::info!("Client disconnected during heartbeat");
                        break;
                    }
                }
                // Broadcast transaction updates
                result = rx.recv() => {
                    match result {
                        Ok(update) => {
                            let json = match serde_json::to_string(&update) {
                                Ok(j) => j,
                                Err(e) => {
                                    tracing::error!("Failed to serialize update: {}", e);
                                    continue;
                                }
                            };

                            if sender.send(Message::Text(json)).await.is_err() {
                                tracing::info!("Client disconnected");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("Client lagged behind by {} messages", n);
                            // Continue serving - client will miss old messages (backpressure handling)
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

    // Wait for either task to finish
    tokio::select! {
        _ = (&mut send_task) => {
            recv_task.abort();
        }
        _ = (&mut recv_task) => {
            send_task.abort();
        }
    }

    tracing::info!("WebSocket connection closed");
}

/// Simple token validation (replace with actual auth logic)
fn validate_token(token: &str) -> bool {
    // TODO: Implement proper token validation
    !token.is_empty()
}
