use crate::error::AppError;
use crate::validation::state_transitions::{is_valid_transition, TRANSACTION_TRANSITIONS};

/// Validates transaction status transitions according to the state machine.
///
/// Valid transitions are defined in `state_transitions::TRANSACTION_TRANSITIONS`.
/// Same-state transitions are always valid (idempotent).
pub fn validate_status_transition(from: &str, to: &str) -> Result<(), AppError> {
    if is_valid_transition(from, to, TRANSACTION_TRANSITIONS) {
        Ok(())
    } else {
        Err(AppError::InvalidStatusTransition(format!(
            "Cannot transition from '{from}' to '{to}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        // From pending
        assert!(validate_status_transition("pending", "processing").is_ok());
        assert!(validate_status_transition("pending", "completed").is_ok());
        assert!(validate_status_transition("pending", "failed").is_ok());

        // From processing
        assert!(validate_status_transition("processing", "completed").is_ok());
        assert!(validate_status_transition("processing", "failed").is_ok());

        // From failed (reprocess)
        assert!(validate_status_transition("failed", "pending").is_ok());

        // Same-state (idempotent)
        assert!(validate_status_transition("pending", "pending").is_ok());
        assert!(validate_status_transition("processing", "processing").is_ok());
        assert!(validate_status_transition("completed", "completed").is_ok());
        assert!(validate_status_transition("failed", "failed").is_ok());
    }

    #[test]
    fn test_invalid_transitions() {
        // Cannot go back from completed
        assert!(validate_status_transition("completed", "pending").is_err());
        assert!(validate_status_transition("completed", "processing").is_err());
        assert!(validate_status_transition("completed", "failed").is_err());

        // Cannot skip from pending to failed without processing
        // (Actually this is valid in our state machine, so this test is removed)

        // Cannot go from processing to pending
        assert!(validate_status_transition("processing", "pending").is_err());

        // Cannot go from failed to processing
        assert!(validate_status_transition("failed", "processing").is_err());
        assert!(validate_status_transition("failed", "completed").is_err());
    }

    #[test]
    fn test_error_message() {
        let result = validate_status_transition("completed", "pending");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("completed"));
        assert!(err.to_string().contains("pending"));
    }
}
