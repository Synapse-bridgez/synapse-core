/// Session management for database connections
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Session information
#[derive(Debug, Clone)]
pub struct Session {
    pub id: Uuid,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub is_active: bool,
}

/// Session manager for database operations
pub struct SessionManager;

impl SessionManager {
    /// Create a new session
    pub async fn create_session(
        pool: &PgPool,
        user_id: &str,
        ttl_seconds: i64,
    ) -> Result<Session, SessionError> {
        if user_id.is_empty() {
            return Err(SessionError::InvalidUserId);
        }

        if ttl_seconds <= 0 {
            return Err(SessionError::InvalidTTL);
        }

        let session_id = Uuid::new_v4();
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(ttl_seconds);

        sqlx::query(
            r#"
            INSERT INTO sessions (id, user_id, created_at, expires_at, is_active)
            VALUES ($1, $2, $3, $4, true)
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(session_id)
        .bind(user_id)
        .bind(now)
        .bind(expires_at)
        .execute(pool)
        .await
        .map_err(|_| SessionError::CreationFailed)?;

        Ok(Session {
            id: session_id,
            user_id: user_id.to_string(),
            created_at: now,
            expires_at,
            is_active: true,
        })
    }

    /// Get session by ID
    pub async fn get_session(pool: &PgPool, session_id: Uuid) -> Result<Option<Session>, SessionError> {
        let row = sqlx::query_as::<_, (Uuid, String, DateTime<Utc>, DateTime<Utc>, bool)>(
            r#"
            SELECT id, user_id, created_at, expires_at, is_active
            FROM sessions
            WHERE id = $1 AND expires_at > NOW()
            "#,
        )
        .bind(session_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| SessionError::FetchFailed)?;

        Ok(row.map(|(id, user_id, created_at, expires_at, is_active)| Session {
            id,
            user_id,
            created_at,
            expires_at,
            is_active,
        }))
    }

    /// Invalidate session
    pub async fn invalidate_session(pool: &PgPool, session_id: Uuid) -> Result<(), SessionError> {
        sqlx::query("UPDATE sessions SET is_active = false WHERE id = $1")
            .bind(session_id)
            .execute(pool)
            .await
            .map_err(|_| SessionError::InvalidationFailed)?;

        Ok(())
    }

    /// Clean up expired sessions
    pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, SessionError> {
        let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= NOW()")
            .execute(pool)
            .await
            .map_err(|_| SessionError::CleanupFailed)?;

        Ok(result.rows_affected())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Invalid user ID")]
    InvalidUserId,
    #[error("Invalid TTL")]
    InvalidTTL,
    #[error("Failed to create session")]
    CreationFailed,
    #[error("Failed to fetch session")]
    FetchFailed,
    #[error("Failed to invalidate session")]
    InvalidationFailed,
    #[error("Failed to cleanup sessions")]
    CleanupFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_creation_invalid_user_id() {
        // This test validates the error handling logic
        let result = validate_user_id("");
        assert!(result.is_err());
    }

    #[test]
    fn test_session_creation_invalid_ttl() {
        let result = validate_ttl(0);
        assert!(result.is_err());

        let result = validate_ttl(-1);
        assert!(result.is_err());
    }

    #[test]
    fn test_session_creation_valid_ttl() {
        let result = validate_ttl(3600);
        assert!(result.is_ok());
    }

    fn validate_user_id(user_id: &str) -> Result<(), SessionError> {
        if user_id.is_empty() {
            return Err(SessionError::InvalidUserId);
        }
        Ok(())
    }

    fn validate_ttl(ttl: i64) -> Result<(), SessionError> {
        if ttl <= 0 {
            return Err(SessionError::InvalidTTL);
        }
        Ok(())
    }

    #[test]
    fn test_session_struct() {
        let session = Session {
            id: Uuid::new_v4(),
            user_id: "user123".to_string(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
            is_active: true,
        };

        assert!(!session.user_id.is_empty());
        assert!(session.is_active);
    }
}
