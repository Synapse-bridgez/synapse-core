//! Reconnection Logic for WebSocket connections
//! 
//! Provides endpoints for clients to manage reconnection state after network interruptions.
//! Includes support for exponential backoff, state recovery, and connection validation.

use axum::{
    extract::{Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::AppState;
use crate::error::AppError;

/// Maximum number of reconnection attempts allowed
const MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Default reconnection timeout in seconds
const DEFAULT_RECONNECT_TIMEOUT: u64 = 30;

/// Maximum reconnect timeout in seconds (cap for exponential backoff)
const MAX_RECONNECT_TIMEOUT: u64 = 300;

// ── Request/Response types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReconnectQuery {
    /// Client's previous connection token for session recovery
    token: Option<String>,
    /// Client's last known sequence number (for gap recovery)
    last_sequence: Option<i64>,
    /// Timestamp of last connection (for stale connection detection)
    last_connected: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconnectStatus {
    /// Client can reconnect immediately
    Ready { session_id: Uuid },
    /// Client must wait before reconnecting (rate limited)
    RetryAfter { wait_seconds: u64 },
    /// Client's session has expired
    SessionExpired,
    /// Client's token is invalid
    InvalidToken,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReconnectionResponse {
    Reconnect {
        status: ReconnectStatus,
        /// Suggested backoff interval for next attempt
        backoff_seconds: u64,
        /// Whether to perform full state resync
        requires_resync: bool,
    },
    Error {
        message: String,
    },
}

// ── Connection state tracking ────────────────────────────────────────────────

/// Tracks client reconnection state in memory
#[derive(Debug, Clone)]
struct ConnectionState {
    session_id: Uuid,
    last_sequence: i64,
    last_connected: i64,
    reconnect_attempts: u32,
    created_at: std::time::Instant,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            session_id: Uuid::new_v4(),
            last_sequence: 0,
            last_connected: chrono::Utc::now().timestamp(),
            reconnect_attempts: 0,
            created_at: std::time::Instant::now(),
        }
    }

    fn can_reconnect(&self) -> bool {
        self.reconnect_attempts < MAX_RECONNECT_ATTEMPTS
    }

    fn increment_attempt(&mut self) {
        self.reconnect_attempts += 1;
    }

    /// Calculate exponential backoff with jitter
    fn calculate_backoff(&self) -> u64 {
        let base = 2_u64.pow(self.reconnect_attempts.min(8));
        let jitter = rand_simple(self.reconnect_attempts);
        (base * DEFAULT_RECONNECT_TIMEOUT / 10).min(MAX_RECONNECT_TIMEOUT) + jitter
    }
}

/// Simple deterministic "random" for jitter (for reproducibility in tests)
fn rand_simple(attempt: u32) -> u64 {
    // Use a simple hash-based approach for jitter
    ((attempt * 31 + 7) % 10) as u64
}

// ── In-memory session store ──────────────────────────────────────────────────

lazy_static::lazy_static! {
    /// Global connection state store
    static ref SESSION_STORE: Arc<Mutex<std::collections::HashMap<Uuid, ConnectionState>>> = 
        Arc::new(Mutex::new(std::collections::HashMap::new()));
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// Check reconnection status for a client
/// 
/// Clients call this endpoint before attempting to reconnect to determine:
/// - If they can reconnect immediately
/// - How long to wait (if rate limited)
/// - If their session has expired
/// 
/// This helps clients avoid hammering the server and implements proper
/// exponential backoff on the client side.
#[utoipa::path(
    get,
    path = "/reconnect/status",
    params(
        ("token" = Option<String>, Query, description = "Client's previous connection token"),
        ("last_sequence" = Option<i64>, Query, description = "Client's last known sequence number"),
        ("last_connected" = Option<i64>, Query, description = "Timestamp of last connection")
    ),
    responses(
        (status = 200, description = "Reconnection status retrieved", body = ReconnectionResponse),
        (status = 401, description = "Invalid or expired token")
    ),
    tag = "WebSocket"
)]
pub async fn reconnect_status(
    Query(query): Query<ReconnectQuery>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    tracing::debug!(
        token_present = query.token.is_some(),
        last_sequence = ?query.last_sequence,
        "Checking reconnection status"
    );

    // Validate token if provided
    if let Some(ref token) = query.token {
        if token.is_empty() || token.len() > 1024 {
            return Ok((
                axum::http::StatusCode::UNAUTHORIZED,
                axum::Json(ReconnectionResponse::Error {
                    message: "Invalid token".to_string(),
                }),
            ));
        }
    }

    // Get current connection state from session store
    let mut store = SESSION_STORE.lock().await;
    
    // For new connections, create a fresh session
    if query.token.is_none() {
        let new_state = ConnectionState::new();
        let session_id = new_state.session_id;
        store.insert(session_id, new_state);
        
        let response = ReconnectionResponse::Reconnect {
            status: ReconnectStatus::Ready { session_id },
            backoff_seconds: 1,
            requires_resync: true,
        };
        
        tracing::info!(session_id = %session_id, "New reconnection session created");
        return Ok((axum::http::StatusCode::OK, axum::Json(response)));
    }

    // Extract session from token (in real impl, this would validate JWT or similar)
    // For this implementation, we treat the token as a session ID
    let session_id = match query.token.as_ref().and_then(|t| Uuid::parse_str(t).ok()) {
        Some(id) => id,
        None => {
            let response = ReconnectionResponse::Reconnect {
                status: ReconnectStatus::InvalidToken,
                backoff_seconds: 0,
                requires_resync: false,
            };
            return Ok((axum::http::StatusCode::OK, axum::Json(response)));
        }
    };

    // Check if session exists
    let session = match store.get(&session_id) {
        Some(s) => s,
        None => {
            let response = ReconnectionResponse::Reconnect {
                status: ReconnectStatus::SessionExpired,
                backoff_seconds: 0,
                requires_resync: false,
            };
            return Ok((axum::http::StatusCode::OK, axum::Json(response)));
        }
    };

    // Check if session is still valid
    if !session.can_reconnect() {
        let response = ReconnectionResponse::Reconnect {
            status: ReconnectStatus::SessionExpired,
            backoff_seconds: MAX_RECONNECT_TIMEOUT,
            requires_resync: false,
        };
        return Ok((axum::http::StatusCode::OK, axum::Json(response)));
    }

    // Determine if resync is needed based on sequence gap
    let requires_resync = match query.last_sequence {
        Some(seq) => seq < session.last_sequence,
        None => true,
    };

    let response = ReconnectionResponse::Reconnect {
        status: ReconnectStatus::Ready { session_id },
        backoff_seconds: session.calculate_backoff(),
        requires_resync,
    };

    tracing::debug!(
        session_id = %session_id,
        requires_resync = requires_resync,
        backoff = session.calculate_backoff(),
        "Reconnection allowed"
    );

    Ok((axum::http::StatusCode::OK, axum::Json(response)))
}

/// Attempt to reconnect a WebSocket client
/// 
/// This endpoint handles the actual reconnection attempt, updating the session
/// state and returning the appropriate response for the client to proceed.
#[utoipa::path(
    post,
    path = "/reconnect",
    request_body = ReconnectRequest,
    responses(
        (status = 200, description = "Reconnection successful", body = ReconnectionResponse),
        (status = 429, description = "Rate limited - too many attempts"),
        (status = 401, description = "Invalid session")
    ),
    tag = "WebSocket"
)]
pub async fn reconnect(
    State(state): State<AppState>,
    axum::Json(payload): axum::Json<ReconnectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session_id = match Uuid::parse_str(&payload.session_id) {
        Ok(id) => id,
        Err(_) => {
            return Ok((
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(ReconnectionResponse::Error {
                    message: "Invalid session ID format".to_string(),
                }),
            ));
        }
    };

    let mut store = SESSION_STORE.lock().await;
    
    let session = match store.get_mut(&session_id) {
        Some(s) => s,
        None => {
            return Ok((
                axum::http::StatusCode::NOT_FOUND,
                axum::Json(ReconnectionResponse::Error {
                    message: "Session not found or expired".to_string(),
                }),
            ));
        }
    };

    // Check reconnect limit
    if !session.can_reconnect() {
        return Ok((
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            axum::Json(ReconnectionResponse::Reconnect {
                status: ReconnectStatus::SessionExpired,
                backoff_seconds: MAX_RECONNECT_TIMEOUT,
                requires_resync: false,
            }),
        ));
    }

    // Update session state
    session.increment_attempt();
    if let Some(seq) = payload.last_sequence {
        session.last_sequence = seq;
    }
    session.last_connected = chrono::Utc::now().timestamp();

    let requires_resync = payload.force_resync.unwrap_or(false);
    let backoff = session.calculate_backoff();

    tracing::info!(
        session_id = %session_id,
        attempt = session.reconnect_attempts,
        "Reconnection attempt processed"
    );

    let response = ReconnectionResponse::Reconnect {
        status: ReconnectStatus::Ready { session_id },
        backoff_seconds: backoff,
        requires_resync,
    };

    Ok((axum::http::StatusCode::OK, axum::Json(response)))
}

#[derive(Debug, Deserialize)]
pub struct ReconnectRequest {
    /// Session ID from previous connection
    session_id: String,
    /// Last sequence number client received
    last_sequence: Option<i64>,
    /// Force full state resync
    force_resync: Option<bool>,
}

/// Clean up stale sessions (called periodically)
pub async fn cleanup_stale_sessions() {
    let mut store = SESSION_STORE.lock().await;
    let now = std::time::Instant::now();
    
    // Remove sessions older than 1 hour
    store.retain(|_, state| {
        now.duration_since(state.created_at).as_secs() < 3600
    });
    
    tracing::debug!(active_sessions = store.len(), "Stale sessions cleaned up");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_creation() {
        let state = ConnectionState::new();
        assert_eq!(state.reconnect_attempts, 0);
        assert!(state.can_reconnect());
    }

    #[test]
    fn test_max_reconnect_attempts() {
        let mut state = ConnectionState::new();
        for _ in 0..MAX_RECONNECT_ATTEMPTS {
            state.increment_attempt();
        }
        assert!(!state.can_reconnect());
    }

    #[test]
    fn test_backoff_increases_with_attempts() {
        let mut state = ConnectionState::new();
        let backoff1 = state.calculate_backoff();
        
        state.increment_attempt();
        let backoff2 = state.calculate_backoff();
        
        // Backoff should increase (or stay same, but not decrease)
        assert!(backoff2 >= backoff1);
    }

    #[test]
    fn test_backoff_capped_at_max() {
        let mut state = ConnectionState::new();
        for _ in 0..20 {
            state.increment_attempt();
        }
        let backoff = state.calculate_backoff();
        assert!(backoff <= MAX_RECONNECT_TIMEOUT);
    }

    #[test]
    fn test_rand_simple_deterministic() {
        let r1 = rand_simple(5);
        let r2 = rand_simple(5);
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_reconnect_query_deserialization() {
        let json = r#"{"token": "abc123", "last_sequence": 100, "last_connected": 1700000000}"#;
        let query: ReconnectQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.token, Some("abc123".to_string()));
        assert_eq!(query.last_sequence, Some(100));
    }

    #[test]
    fn test_reconnect_query_optional_fields() {
        let json = r#"{}"#;
        let query: ReconnectQuery = serde_json::from_str(json).unwrap();
        assert_eq!(query.token, None);
        assert_eq!(query.last_sequence, None);
    }

    #[test]
    fn test_reconnect_request_deserialization() {
        let json = r#"{"session_id": "550e8400-e29b-41d4-a716-446655440000", "last_sequence": 50}"#;
        let req: ReconnectRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.last_sequence, Some(50));
    }

    #[test]
    fn test_reconnect_status_ready_serialization() {
        let session_id = Uuid::new_v4();
        let status = ReconnectStatus::Ready { session_id };
        let response = ReconnectionResponse::Reconnect {
            status,
            backoff_seconds: 5,
            requires_resync: true,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("ready"));
        assert!(json.contains("backoff_seconds"));
    }

    #[test]
    fn test_reconnect_status_session_expired_serialization() {
        let status = ReconnectStatus::SessionExpired;
        let response = ReconnectionResponse::Reconnect {
            status,
            backoff_seconds: 0,
            requires_resync: false,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("session_expired"));
    }

    #[test]
    fn test_reconnect_status_retry_after_serialization() {
        let status = ReconnectStatus::RetryAfter { wait_seconds: 30 };
        let response = ReconnectionResponse::Reconnect {
            status,
            backoff_seconds: 30,
            requires_resync: false,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("retry_after"));
    }
}