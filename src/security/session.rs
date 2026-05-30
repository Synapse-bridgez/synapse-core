/// Secure Session Management — validation and rate-limiting checks.
use chrono::{DateTime, Utc};
use uuid::Uuid;

const MAX_SESSION_TTL_SECS: i64 = 86_400; // 24 h
const MAX_USER_ID_LEN: usize = 128;

#[derive(Debug, thiserror::Error)]
pub enum SessionValidationError {
    #[error("User ID is empty")]
    EmptyUserId,
    #[error("User ID exceeds maximum length")]
    UserIdTooLong,
    #[error("TTL must be between 1 and {MAX_SESSION_TTL_SECS} seconds")]
    InvalidTtl,
    #[error("Session has expired")]
    Expired,
    #[error("Session is inactive")]
    Inactive,
}

/// Lightweight session record used for validation.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub is_active: bool,
}

/// Validate inputs before creating a session.
pub fn validate_session_params(user_id: &str, ttl_seconds: i64) -> Result<(), SessionValidationError> {
    if user_id.is_empty() {
        return Err(SessionValidationError::EmptyUserId);
    }
    if user_id.len() > MAX_USER_ID_LEN {
        return Err(SessionValidationError::UserIdTooLong);
    }
    if !(1..=MAX_SESSION_TTL_SECS).contains(&ttl_seconds) {
        return Err(SessionValidationError::InvalidTtl);
    }
    Ok(())
}

/// Validate that an existing session is still usable.
pub fn validate_session(session: &SessionRecord) -> Result<(), SessionValidationError> {
    if !session.is_active {
        return Err(SessionValidationError::Inactive);
    }
    if session.expires_at <= Utc::now() {
        return Err(SessionValidationError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_valid_params() {
        assert!(validate_session_params("user123", 3600).is_ok());
    }

    #[test]
    fn test_empty_user_id() {
        assert!(matches!(
            validate_session_params("", 3600),
            Err(SessionValidationError::EmptyUserId)
        ));
    }

    #[test]
    fn test_user_id_too_long() {
        let long_id = "a".repeat(MAX_USER_ID_LEN + 1);
        assert!(matches!(
            validate_session_params(&long_id, 3600),
            Err(SessionValidationError::UserIdTooLong)
        ));
    }

    #[test]
    fn test_invalid_ttl_zero() {
        assert!(matches!(
            validate_session_params("user", 0),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_invalid_ttl_negative() {
        assert!(matches!(
            validate_session_params("user", -1),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_invalid_ttl_exceeds_max() {
        assert!(matches!(
            validate_session_params("user", MAX_SESSION_TTL_SECS + 1),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_valid_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() + Duration::hours(1),
            is_active: true,
        };
        assert!(validate_session(&session).is_ok());
    }

    #[test]
    fn test_expired_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() - Duration::seconds(1),
            is_active: true,
        };
        assert!(matches!(
            validate_session(&session),
            Err(SessionValidationError::Expired)
        ));
    }

    #[test]
    fn test_inactive_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() + Duration::hours(1),
            is_active: false,
        };
        assert!(matches!(
            validate_session(&session),
            Err(SessionValidationError::Inactive)
        ));
    }
}
