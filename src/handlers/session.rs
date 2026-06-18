//! Session management for authenticated API requests.
//!
//! Provides session creation, lookup, and invalidation with Redis-backed storage.
//! Each session has a UUID, user ID, creation timestamp, and configurable expiry.

use axum::{
    async_trait,
    extract::FromRequest,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

const SESSION_PREFIX: &str = "session:";
const SESSION_HEADER: &str = "X-Session-ID";

/// Session data stored in Redis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Unique session identifier.
    pub id: Uuid,
    /// Authenticated user ID.
    pub user_id: String,
    /// Session creation timestamp (milliseconds since epoch).
    pub created_at: u64,
    /// Session expiry timestamp (milliseconds since epoch).
    pub expires_at: u64,
}

impl Session {
    /// Checks if the session is expired.
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        now > self.expires_at
    }
}

/// Configuration for session management.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Session expiry duration (default: 24 hours).
    pub expiry: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            expiry: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Session manager for creating, looking up, and invalidating sessions.
#[derive(Clone)]
pub struct SessionManager {
    redis: ConnectionManager,
    config: SessionConfig,
}

impl SessionManager {
    /// Creates a new session manager.
    pub fn new(redis: ConnectionManager, config: SessionConfig) -> Self {
        Self { redis, config }
    }

    /// Creates a new session for the authenticated user.
    ///
    /// Returns the created session with a unique ID and configured expiry.
    pub async fn create_session(&self, user_id: String) -> Result<Session, SessionError> {
        let session_id = Uuid::new_v4();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| SessionError::SystemTime)?
            .as_millis() as u64;
        let expires_at = now + self.config.expiry.as_millis() as u64;

        let session = Session {
            id: session_id,
            user_id,
            created_at: now,
            expires_at,
        };

        let key = format!("{}{}", SESSION_PREFIX, session_id);
        // Redis rejects an `EX` of 0, so floor the TTL at 1 second. Sub-second
        // expiries (used in tests) are still enforced via `expires_at`/`is_expired`.
        let ttl = (self.config.expiry.as_secs() as usize).max(1);

        let serialized =
            serde_json::to_string(&session).map_err(|_| SessionError::SerializationError)?;

        redis::cmd("SET")
            .arg(&key)
            .arg(&serialized)
            .arg("EX")
            .arg(ttl)
            .query_async::<_, ()>(&mut self.redis.clone())
            .await
            .map_err(|_| SessionError::RedisError)?;

        Ok(session)
    }

    /// Looks up a session by ID.
    ///
    /// Returns the session if it exists and is not expired. Returns `SessionError::NotFound`
    /// if the session does not exist or has expired.
    pub async fn lookup_session(&self, session_id: &Uuid) -> Result<Session, SessionError> {
        let key = format!("{}{}", SESSION_PREFIX, session_id);

        let value: Option<String> = redis::cmd("GET")
            .arg(&key)
            .query_async(&mut self.redis.clone())
            .await
            .map_err(|_| SessionError::RedisError)?;

        let serialized = value.ok_or(SessionError::NotFound)?;
        let session: Session =
            serde_json::from_str(&serialized).map_err(|_| SessionError::DeserializationError)?;

        if session.is_expired() {
            return Err(SessionError::Expired);
        }

        Ok(session)
    }

    /// Invalidates a session by deleting it from Redis.
    pub async fn invalidate_session(&self, session_id: &Uuid) -> Result<(), SessionError> {
        let key = format!("{}{}", SESSION_PREFIX, session_id);

        redis::cmd("DEL")
            .arg(&key)
            .query_async::<_, ()>(&mut self.redis.clone())
            .await
            .map_err(|_| SessionError::RedisError)?;

        Ok(())
    }
}

/// Error types for session operations.
#[derive(Debug, Clone, Copy)]
pub enum SessionError {
    /// Session not found in Redis.
    NotFound,
    /// Session has expired.
    Expired,
    /// Redis operation failed.
    RedisError,
    /// Serialization/deserialization error.
    SerializationError,
    DeserializationError,
    /// System time error.
    SystemTime,
    /// Missing session header.
    MissingHeader,
}

impl IntoResponse for SessionError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            SessionError::NotFound | SessionError::Expired => {
                (StatusCode::UNAUTHORIZED, "Invalid or expired session")
            }
            SessionError::MissingHeader => (StatusCode::UNAUTHORIZED, "Missing session ID header"),
            SessionError::RedisError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Session service error")
            }
            SessionError::SerializationError | SessionError::DeserializationError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Session data error")
            }
            SessionError::SystemTime => (StatusCode::INTERNAL_SERVER_ERROR, "System time error"),
        };

        tracing::warn!("Session error: {:?}", self);
        (status, message).into_response()
    }
}

/// Axum extractor for extracting session from request headers.
///
/// Extracts the session ID from `X-Session-ID` header and looks up the session in Redis.
/// Returns 401 if the header is missing, session not found, or session is expired.
pub struct SessionContext {
    pub session: Session,
}

#[async_trait]
impl<S, B> FromRequest<S, B> for SessionContext
where
    S: Send + Sync + 'static,
    B: Send + 'static,
{
    type Rejection = SessionError;

    async fn from_request(req: Request<B>, _state: &S) -> Result<Self, Self::Rejection> {
        let headers = req.headers();

        // Extract session ID from header.
        let session_id_str = headers
            .get(SESSION_HEADER)
            .and_then(|h| h.to_str().ok())
            .ok_or(SessionError::MissingHeader)?;

        let session_id = Uuid::parse_str(session_id_str).map_err(|_| SessionError::NotFound)?;

        // Create a session manager (in a real app, this would come from AppState).
        // For now, we'll create a temporary one for this test.
        // In production, SessionManager should be part of AppState.
        let redis_url = "redis://localhost:6379";
        let client = redis::Client::open(redis_url).map_err(|_| SessionError::RedisError)?;
        let manager = ConnectionManager::new(client)
            .await
            .map_err(|_| SessionError::RedisError)?;

        let session_manager = SessionManager::new(manager, SessionConfig::default());

        // Look up the session.
        let session = session_manager.lookup_session(&session_id).await?;

        Ok(SessionContext { session })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[tokio::test]
    #[ignore = "requires a running Redis instance"]
    async fn test_session_creation() {
        let client = redis::Client::open("redis://localhost:6379").unwrap();
        let manager = ConnectionManager::new(client).await.unwrap();
        let session_mgr = SessionManager::new(manager, SessionConfig::default());

        let session = session_mgr
            .create_session("user123".to_string())
            .await
            .unwrap();

        assert_eq!(session.user_id, "user123");
        assert!(!session.is_expired());
    }

    #[tokio::test]
    #[ignore = "requires a running Redis instance"]
    async fn test_session_lookup_succeeds() {
        let client = redis::Client::open("redis://localhost:6379").unwrap();
        let manager = ConnectionManager::new(client).await.unwrap();
        let session_mgr = SessionManager::new(manager, SessionConfig::default());

        let created = session_mgr
            .create_session("user456".to_string())
            .await
            .unwrap();
        let looked_up = session_mgr.lookup_session(&created.id).await.unwrap();

        assert_eq!(created.id, looked_up.id);
        assert_eq!(created.user_id, looked_up.user_id);
    }

    #[tokio::test]
    #[ignore = "requires a running Redis instance"]
    async fn test_expired_session_returns_error() {
        let client = redis::Client::open("redis://localhost:6379").unwrap();
        let manager = ConnectionManager::new(client).await.unwrap();

        let config = SessionConfig {
            expiry: Duration::from_millis(1),
        };
        let session_mgr = SessionManager::new(manager, config);

        let session = session_mgr
            .create_session("user789".to_string())
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = session_mgr.lookup_session(&session.id).await;
        assert!(matches!(result, Err(SessionError::Expired)));
    }

    #[tokio::test]
    #[ignore = "requires a running Redis instance"]
    async fn test_invalidated_session_returns_not_found() {
        let client = redis::Client::open("redis://localhost:6379").unwrap();
        let manager = ConnectionManager::new(client).await.unwrap();
        let session_mgr = SessionManager::new(manager, SessionConfig::default());

        let session = session_mgr
            .create_session("user999".to_string())
            .await
            .unwrap();

        session_mgr.invalidate_session(&session.id).await.unwrap();

        let result = session_mgr.lookup_session(&session.id).await;
        assert!(matches!(result, Err(SessionError::NotFound)));
    }

    #[test]
    fn test_missing_session_header() {
        let headers = HeaderMap::new();
        let session_id = headers.get(SESSION_HEADER);
        assert!(session_id.is_none());
    }
}
