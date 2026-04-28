use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Query, State,
    },
    response::IntoResponse,
};
use futures::{sink::SinkExt, stream::StreamExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::AppState;

/// How often to send a ping frame to the client.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// How long to wait for a pong before closing the connection.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

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
    // ConnectInfo is optional — only present when using into_make_service_with_connect_info
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    if let Some(token) = params.token {
        if !validate_token(&token) {
            tracing::warn!("Invalid WebSocket authentication token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    }

    let client_addr = connect_info.map(|ci| ci.0.to_string()).unwrap_or_else(|| "unknown".to_string());

    ws.on_upgrade(move |socket| handle_socket(socket, state, client_addr))
}

/// Handle individual WebSocket connection with heartbeat and pong tracking.
async fn handle_socket(socket: WebSocket, state: AppState, client_addr: String) {
    // Increment active connection counter
    let count = state.ws_connection_count.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(client_addr = %client_addr, active_connections = count, "WebSocket connection opened");

    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    // Shared flag: did we receive a pong since the last ping?
    let pong_received = Arc::new(std::sync::atomic::AtomicBool::new(true));

    let mut rx = state.tx_broadcast.subscribe();

    // ── Receive task ────────────────────────────────────────────────────────
    let pong_flag = Arc::clone(&pong_received);
    let recv_addr = client_addr.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    tracing::debug!(client_addr = %recv_addr, "Received text: {}", text);
                }
                Message::Pong(_) => {
                    tracing::trace!(client_addr = %recv_addr, "Received pong");
                    pong_flag.store(true, Ordering::Relaxed);
                }
                Message::Ping(_) => {
                    tracing::trace!(client_addr = %recv_addr, "Received ping (axum handles pong)");
                }
                Message::Close(_) => {
                    tracing::info!(client_addr = %recv_addr, "Client sent close frame");
                    break;
                }
                _ => {}
            }
        }
    });

    // ── Send task (heartbeat + broadcast) ───────────────────────────────────
    let sender_clone = Arc::clone(&sender);
    let pong_flag2 = Arc::clone(&pong_received);
    let send_addr = client_addr.clone();
    let mut send_task = tokio::spawn(async move {
        let mut heartbeat_interval = tokio::time::interval(HEARTBEAT_INTERVAL);

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    // Check that the previous ping was answered
                    if !pong_flag2.swap(false, Ordering::Relaxed) {
                        tracing::warn!(
                            client_addr = %send_addr,
                            "No pong received within {}s — closing dead connection",
                            PONG_TIMEOUT.as_secs()
                        );
                        break;
                    }

                    // Send ping; give the client PONG_TIMEOUT to reply
                    let send_result = {
                        let mut s = sender_clone.lock().await;
                        timeout(PONG_TIMEOUT, s.send(Message::Ping(vec![]))).await
                    };

                    match send_result {
                        Ok(Ok(())) => {
                            tracing::trace!(client_addr = %send_addr, "Sent ping");
                        }
                        Ok(Err(_)) | Err(_) => {
                            tracing::info!(client_addr = %send_addr, "Client disconnected during heartbeat");
                            break;
                        }
                    }
                }

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
                            let mut s = sender_clone.lock().await;
                            if s.send(Message::Text(json)).await.is_err() {
                                tracing::info!(client_addr = %send_addr, "Client disconnected while sending update");
                                break;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(client_addr = %send_addr, "Client lagged by {} messages", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::info!(client_addr = %send_addr, "Broadcast channel closed");
                            break;
                        }
                    }
                }
            }
        }
    });

    // Wait for either task to finish, then abort the other
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }

    // Decrement active connection counter
    let remaining = state.ws_connection_count.fetch_sub(1, Ordering::Relaxed) - 1;
    tracing::info!(
        client_addr = %client_addr,
        active_connections = remaining,
        "WebSocket connection closed"
    );
}

/// Simple token validation (replace with actual auth logic)
fn validate_token(token: &str) -> bool {
    !token.is_empty()
}
