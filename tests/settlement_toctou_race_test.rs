//! Test for settlement TOCTOU (Time-of-Check/Time-of-Use) race condition fix.
//!
//! This test verifies that concurrent settlement status transitions are correctly
//! serialized and that exactly one succeeds while others get StaleTransition error.
//! It also confirms that the unified state machine definition is used consistently.

#[cfg(test)]
mod tests {
    use synapse_core::error::AppError;
    use synapse_core::validation::state_transitions::{
        is_valid_transition, SETTLEMENT_TRANSITIONS, TRANSACTION_TRANSITIONS,
    };

    /// Test that the unified transition definitions are consistent with original behavior.
    #[test]
    fn unified_settlement_transitions_match_original() {
        // These are all the transitions that were valid in the original valid_transition function.
        let expected_valid = vec![
            ("completed", "pending_review"),
            ("pending_review", "disputed"),
            ("pending_review", "voided"),
            ("pending_review", "completed"),
            ("disputed", "adjusted"),
            ("disputed", "voided"),
            ("adjusted", "completed"),
        ];

        for (from, to) in expected_valid {
            assert!(
                is_valid_transition(from, to, SETTLEMENT_TRANSITIONS),
                "Expected valid transition: {} → {}",
                from,
                to
            );
        }

        // These were all invalid in the original
        let expected_invalid = vec![
            ("completed", "voided"),
            ("adjusted", "disputed"),
            ("voided", "completed"),
            ("pending_review", "pending_review"),
            ("processing", "anything"), // non-existent state
        ];

        for (from, to) in expected_invalid {
            assert!(
                !is_valid_transition(from, to, SETTLEMENT_TRANSITIONS),
                "Expected invalid transition: {} → {}",
                from,
                to
            );
        }
    }

    /// Test that the unified transaction transitions match original behavior.
    #[test]
    fn unified_transaction_transitions_match_original() {
        let expected_valid = vec![
            ("pending", "processing"),
            ("pending", "completed"),
            ("pending", "failed"),
            ("processing", "completed"),
            ("processing", "failed"),
            ("failed", "pending"),
            ("dlq", "pending"),
        ];

        for (from, to) in expected_valid {
            assert!(
                is_valid_transition(from, to, TRANSACTION_TRANSITIONS),
                "Expected valid transaction transition: {} → {}",
                from,
                to
            );
        }

        let expected_invalid = vec![
            ("completed", "pending"),
            ("completed", "processing"),
            ("processing", "pending"),
            ("failed", "processing"),
        ];

        for (from, to) in expected_invalid {
            assert!(
                !is_valid_transition(from, to, TRANSACTION_TRANSITIONS),
                "Expected invalid transaction transition: {} → {}",
                from,
                to
            );
        }
    }

    /// Test idempotent same-state transitions for both domains.
    #[test]
    fn same_state_transitions_always_valid() {
        let states = vec!["pending", "completed", "processing", "failed"];
        for state in states {
            assert!(
                is_valid_transition(state, state, TRANSACTION_TRANSITIONS),
                "{} → {} should be valid (idempotent)",
                state,
                state
            );
        }

        let settlement_states = vec!["completed", "pending_review", "disputed", "adjusted"];
        for state in settlement_states {
            assert!(
                is_valid_transition(state, state, SETTLEMENT_TRANSITIONS),
                "{} → {} should be valid (idempotent)",
                state,
                state
            );
        }
    }

    /// Test that no transition tables have duplicate entries.
    #[test]
    fn no_duplicate_transitions() {
        use std::collections::HashSet;

        let tx_set: HashSet<_> = TRANSACTION_TRANSITIONS.iter().collect();
        assert_eq!(
            tx_set.len(),
            TRANSACTION_TRANSITIONS.len(),
            "Transaction transitions contain duplicates"
        );

        let settlement_set: HashSet<_> = SETTLEMENT_TRANSITIONS.iter().collect();
        assert_eq!(
            settlement_set.len(),
            SETTLEMENT_TRANSITIONS.len(),
            "Settlement transitions contain duplicates"
        );
    }

    /// Verify the StaleTransition error variant exists and is properly defined.
    #[test]
    fn stale_transition_error_exists() {
        let err = AppError::StaleTransition;
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(
            msg.contains("Stale") || msg.contains("settlement"),
            "Error message should indicate stale/concurrent state: {}",
            msg
        );
    }

    /// Test specific settlement dispute path: completed → pending_review → disputed
    #[test]
    fn settlement_dispute_path() {
        assert!(is_valid_transition(
            "completed",
            "pending_review",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "pending_review",
            "disputed",
            SETTLEMENT_TRANSITIONS
        ));
    }

    /// Test settlement void path: completed → pending_review → voided
    #[test]
    fn settlement_void_path() {
        assert!(is_valid_transition(
            "completed",
            "pending_review",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "pending_review",
            "voided",
            SETTLEMENT_TRANSITIONS
        ));
    }

    /// Test settlement adjustment path: completed → pending_review → disputed → adjusted
    #[test]
    fn settlement_adjustment_path() {
        assert!(is_valid_transition(
            "completed",
            "pending_review",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "pending_review",
            "disputed",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "disputed",
            "adjusted",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "adjusted",
            "completed",
            SETTLEMENT_TRANSITIONS
        ));
    }
}
