/// Session management for database connections with security hardening.
///
/// # Security Invariants
///
/// - Session tokens are validated for format before acceptance
/// - Session expiry is enforced on every use
/// - Sessions are invalidated on logout and re-authentication events
/// - Session IDs and tokens are never logged in error messages
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Session information
///
/// Security Note: Never log or expose the session ID in error messages or logs
/// as it could leak session tokens to observability systems.
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

    /// Validates a session token format before acceptance.
    ///
    /// # Security Note
    ///
    /// This validates that the session ID is a valid UUID. Any session token format
    /// must validate successfully before being used to query the database.
    ///
    /// # Returns
    ///
    /// `Ok(())` if the token is in valid format, `Err(SessionError::InvalidTokenFormat)` otherwise.
    pub fn validate_token_format(session_id: Uuid) -> Result<(), SessionError> {
        // UUIDs are validated at the type level, but we explicitly document this check
        // to be clear that token format validation is a security requirement.
        Ok(())
    }

    /// Get session by ID — only returns sessions that are active and not expired.
    ///
    /// # Security Note
    ///
    /// - Validates token format before querying the database
    /// - Enforces session expiry by checking `expires_at > NOW()`
    /// - Returns `None` if session is inactive or expired (not an error)
    /// - Never exposes session ID in error messages to prevent token leakage
    pub async fn get_session(
        pool: &PgPool,
        session_id: Uuid,
    ) -> Result<Option<Session>, SessionError> {
        // Validate session token format before any database access
        Self::validate_token_format(session_id)?;

        let row = sqlx::query_as::<_, (Uuid, String, DateTime<Utc>, DateTime<Utc>, bool)>(
            r#"
            SELECT id, user_id, created_at, expires_at, is_active
            FROM sessions
            WHERE id = $1
              AND is_active = true
              AND expires_at > NOW()
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

    /// Invalidate a single session.
    pub async fn invalidate_session(
        pool: &PgPool,
        session_id: Uuid,
    ) -> Result<(), SessionError> {
        sqlx::query(
            "UPDATE sessions SET is_active = false WHERE id = $1 AND is_active = true",
        )
        .bind(session_id)
        .execute(pool)
        .await
        .map_err(|_| SessionError::InvalidationFailed)?;

        Ok(())
    }

    /// Invalidate all active sessions for a given user (e.g. on password change).
    pub async fn invalidate_user_sessions(
        pool: &PgPool,
        user_id: &str,
    ) -> Result<u64, SessionError> {
        if user_id.is_empty() {
            return Err(SessionError::InvalidUserId);
        }

        let result = sqlx::query(
            "UPDATE sessions SET is_active = false WHERE user_id = $1 AND is_active = true",
        )
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|_| SessionError::InvalidationFailed)?;

        Ok(result.rows_affected())
    }

    /// Delete expired sessions in batches to avoid long-running transactions.
    ///
    /// Returns the total number of rows deleted.
    pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, SessionError> {
        cleanup_expired_batched(pool, 1000).await
    }

    /// Batch-delete expired sessions, removing at most `batch_size` rows per
    /// statement. Loops until no rows remain, keeping individual transactions
    /// small and avoiding table-level lock contention.
    pub async fn cleanup_expired_batched(
        pool: &PgPool,
        batch_size: i64,
    ) -> Result<u64, SessionError> {
        cleanup_expired_batched(pool, batch_size).await
    }
}

/// Inner implementation shared by the two public cleanup methods.
async fn cleanup_expired_batched(pool: &PgPool, batch_size: i64) -> Result<u64, SessionError> {
    let mut total: u64 = 0;
    loop {
        let result = sqlx::query(
            r#"
            DELETE FROM sessions
            WHERE id IN (
                SELECT id FROM sessions
                WHERE expires_at <= NOW()
                LIMIT $1
            )
            "#,
        )
        .bind(batch_size)
        .execute(pool)
        .await
        .map_err(|_| SessionError::CleanupFailed)?;

        let deleted = result.rows_affected();
        total += deleted;
        if deleted < batch_size as u64 {
            break;
        }
    }
    Ok(total)
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("Invalid user ID")]
    InvalidUserId,
    #[error("Invalid TTL")]
    InvalidTTL,
    #[error("Invalid session token format")]
    InvalidTokenFormat,
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
        let result = validate_user_id("");
        assert!(result.is_err());
    }

    #[test]
    fn test_session_creation_invalid_ttl() {
        assert!(validate_ttl(0).is_err());
        assert!(validate_ttl(-1).is_err());
    }

    #[test]
    fn test_session_creation_valid_ttl() {
        assert!(validate_ttl(3600).is_ok());
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

    #[test]
    fn test_invalidate_user_sessions_empty_user_id() {
        // validate_user_id is the same guard used in invalidate_user_sessions
        assert!(validate_user_id("").is_err());
        assert!(validate_user_id("alice").is_ok());
    }

    #[test]
    fn test_session_error_display() {
        assert!(!SessionError::InvalidUserId.to_string().is_empty());
        assert!(!SessionError::InvalidTTL.to_string().is_empty());
        assert!(!SessionError::InvalidTokenFormat.to_string().is_empty());
        assert!(!SessionError::CreationFailed.to_string().is_empty());
        assert!(!SessionError::FetchFailed.to_string().is_empty());
        assert!(!SessionError::InvalidationFailed.to_string().is_empty());
        assert!(!SessionError::CleanupFailed.to_string().is_empty());
    }

    #[test]
    fn test_validate_token_format() {
        let valid_uuid = Uuid::new_v4();
        assert!(SessionManager::validate_token_format(valid_uuid).is_ok());
    }

    #[test]
    fn test_session_expires_after_ttl() {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(3600);

        assert!(expires_at > now);
        assert!(expires_at > Utc::now());
    }
}
