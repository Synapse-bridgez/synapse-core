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
use crate::health::DependencySeverity;

use crate::handlers::ws_error::{validate_ws_token, validate_message_size, validate_message_structure};

/// How often to send a ping frame to the client.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// How long to wait for a pong before closing the connection.
const PONG_TIMEOUT: Duration = Duration::from_secs(10);

/// Default number of events returned on a resync request.
const RESYNC_DEFAULT_LIMIT: i64 = 20;

/// Maximum number of events a client may request in a single resync.
const RESYNC_MAX_LIMIT: i64 = 100;

// ── Wire types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, async_graphql::SimpleObject)]
pub struct TransactionStatusUpdate {
    pub transaction_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub message: Option<String>,
}

/// Messages the server pushes to the client.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    /// Notification that messages were dropped due to the client being slow.
    MessagesDropped { count: u64 },
    /// Response to a client `resync` request — latest N events from the DB.
    Resync {
        events: Vec<crate::db::models::Transaction>,
    },
}

/// Messages the client may send to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Ask for the latest `limit` events (defaults to [`RESYNC_DEFAULT_LIMIT`]).
    Resync { limit: Option<i64> },
}

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    token: Option<String>,
}

// ── Upgrade handler ──────────────────────────────────────────────────────────

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    let token = match params.token {
        Some(t) => match validate_ws_token(&t) {
            Ok(_) => t,
            Err(_) => {
                tracing::warn!("Invalid WebSocket authentication token");
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
        },
        None => {
            tracing::warn!("Missing WebSocket authentication token");
            return axum::http::StatusCode::UNAUTHORIZED.into_response();
        }
    };

    let client_addr = connect_info
        .map(|ci| ci.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let _ = token; // validated above
    ws.on_upgrade(move |socket| handle_socket(socket, state, client_addr))
}

// ── Per-connection handler ───────────────────────────────────────────────────

async fn handle_socket(socket: WebSocket, state: AppState, client_addr: String) {
    let count = state.ws_connection_count.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(
        client_addr = %client_addr,
        active_connections = count,
        "WebSocket connection opened"
    );

    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(Mutex::new(sender));

    // Shared flag: did we receive a pong since the last ping?
    let pong_received = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // Per-client dropped-message counter (metric).
    let messages_dropped_total = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let mut rx = state.tx_broadcast.subscribe();

    // ── Receive task ─────────────────────────────────────────────────────────
    let pong_flag = Arc::clone(&pong_received);
    let recv_addr = client_addr.clone();
    let recv_sender = Arc::clone(&sender);
    let recv_state = state.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = receiver.next().await {
            match msg {
                Message::Text(text) => {
                    tracing::debug!(client_addr = %recv_addr, "Received text: {}", text);
                    handle_client_message(&text, &recv_sender, &recv_state, &recv_addr).await;
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

    // ── Send task (heartbeat + broadcast + backpressure) ─────────────────────
    let sender_clone = Arc::clone(&sender);
    let pong_flag2 = Arc::clone(&pong_received);
    let dropped_counter = Arc::clone(&messages_dropped_total);
    let send_addr = client_addr.clone();
    let mut send_task = tokio::spawn(async move {
        let mut heartbeat_interval = tokio::time::interval(HEARTBEAT_INTERVAL);

        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    if !pong_flag2.swap(false, Ordering::Relaxed) {
                        tracing::warn!(
                            client_addr = %send_addr,
                            "No pong received within {}s — closing dead connection",
                            PONG_TIMEOUT.as_secs()
                        );
                        break;
                    }

                    let send_result = {
                        let mut s = sender_clone.lock().await;
                        timeout(PONG_TIMEOUT, s.send(Message::Ping(vec![]))).await
                    };

                    match send_result {
                        Ok(Ok(())) => tracing::trace!(client_addr = %send_addr, "Sent ping"),
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

                        // ── Backpressure: client is too slow ─────────────
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            let total = dropped_counter.fetch_add(n, Ordering::Relaxed) + n;
                            tracing::warn!(
                                client_addr = %send_addr,
                                dropped = n,
                                ws_messages_dropped_total = total,
                                "Client lagged — sending messages_dropped notification"
                            );

                            let notification = ServerMessage::MessagesDropped { count: n };
                            if let Ok(json) = serde_json::to_string(&notification) {
                                let mut s = sender_clone.lock().await;
                                // Best-effort: ignore send error here, the next recv will catch a dead socket
                                let _ = s.send(Message::Text(json)).await;
                            }
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

    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }

    let remaining = state.ws_connection_count.fetch_sub(1, Ordering::Relaxed) - 1;
    let total_dropped = messages_dropped_total.load(Ordering::Relaxed);
    tracing::info!(
        client_addr = %client_addr,
        active_connections = remaining,
        ws_messages_dropped_total = total_dropped,
        "WebSocket connection closed"
    );
}

// ── Client message handler ───────────────────────────────────────────────────

async fn handle_client_message(
    text: &str,
    sender: &Arc<Mutex<impl SinkExt<Message, Error = axum::Error> + Unpin + Send>>,
    state: &AppState,
    client_addr: &str,
) {
    // Validate message size first
    if let Err(e) = validate_message_size(text) {
        tracing::warn!(
            client_addr = %client_addr,
            error = %e,
            "Message size validation failed"
        );
        return;
    }

    // Validate message structure
    let msg: ClientMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(_) => {
            tracing::debug!(client_addr = %client_addr, "Ignoring unparseable client message");
            return;
        }
    };

    match msg {
        ClientMessage::Resync { limit } => {
            let limit = limit
                .unwrap_or(RESYNC_DEFAULT_LIMIT)
                .clamp(1, RESYNC_MAX_LIMIT);

            tracing::info!(
                client_addr = %client_addr,
                limit = limit,
                "Client requested resync"
            );

            let events = match crate::db::queries::list_transactions(&state.db, limit, None, false)
                .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::error!(client_addr = %client_addr, "Resync DB query failed: {}", e);
                    return;
                }
            };

            let response = ServerMessage::Resync { events };
            if let Ok(json) = serde_json::to_string(&response) {
                let mut s = sender.lock().await;
                let _ = s.send(Message::Text(json)).await;
            }
        }
    }
}

// ── Token validation ─────────────────────────────────────────────────────────

fn validate_token(token: &str) -> bool {
    !token.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_token_empty() {
        assert!(!validate_token(""));
    }

    #[test]
    fn test_validate_token_valid() {
        assert!(validate_token("valid_token_123"));
    }

    #[test]
    fn test_validate_token_whitespace() {
        assert!(validate_token(" "));
    }

    #[test]
    fn test_heartbeat_interval_constant() {
        assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn test_pong_timeout_constant() {
        assert_eq!(PONG_TIMEOUT, Duration::from_secs(10));
    }

    #[test]
    fn test_resync_default_limit() {
        assert_eq!(RESYNC_DEFAULT_LIMIT, 20);
    }

    #[test]
    fn test_resync_max_limit() {
        assert_eq!(RESYNC_MAX_LIMIT, 100);
    }

    #[test]
    fn test_client_message_resync_deserialization() {
        let json = r#"{"type": "resync", "limit": 50}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Resync { limit } => {
                assert_eq!(limit, Some(50));
            }
        }
    }

    #[test]
    fn test_client_message_resync_no_limit() {
        let json = r#"{"type": "resync"}"#;
        let msg: ClientMessage = serde_json::from_str(json).unwrap();
        match msg {
            ClientMessage::Resync { limit } => {
                assert_eq!(limit, None);
            }
        }
    }

    #[test]
    fn test_server_message_messages_dropped_serialization() {
        let msg = ServerMessage::MessagesDropped { count: 42 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("messages_dropped"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_transaction_status_update_serialization() {
        let update = TransactionStatusUpdate {
            transaction_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            status: "completed".to_string(),
            timestamp: chrono::Utc::now(),
            message: Some("Transaction processed".to_string()),
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("completed"));
        assert!(json.contains("Transaction processed"));
    }

    #[test]
    fn test_ws_query_token_present() {
        let json = r#"{"token": "test_token"}"#;
        let query: WsQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.token, Some("test_token".to_string()));
    }

    #[test]
    fn test_ws_query_token_absent() {
        let json = r#"{}"#;
        let query: WsQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.token, None);
    }

    #[test]
    fn test_resync_limit_clamping_below_min() {
        let limit = 0i64.clamp(1, RESYNC_MAX_LIMIT);
        assert_eq!(limit, 1);
    }

    #[test]
    fn test_resync_limit_clamping_above_max() {
        let limit = 200i64.clamp(1, RESYNC_MAX_LIMIT);
        assert_eq!(limit, RESYNC_MAX_LIMIT);
    }

    #[test]
    fn test_resync_limit_clamping_within_range() {
        let limit = 50i64.clamp(1, RESYNC_MAX_LIMIT);
        assert_eq!(limit, 50);
    }

    #[test]
    fn test_dependency_severity_critical() {
        let severity = DependencySeverity::Critical;
        assert_eq!(severity, DependencySeverity::Critical);
    }

    #[test]
    fn test_dependency_severity_non_critical() {
        let severity = DependencySeverity::NonCritical;
        assert_eq!(severity, DependencySeverity::NonCritical);
    }
}
