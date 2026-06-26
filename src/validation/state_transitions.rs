//! Declarative state transition table consumed by all domains.
//! Each domain builds its allowed transitions from these definitions.

/// A single allowed transition from one state to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Transition {
    pub from: &'static str,
    pub to: &'static str,
}

/// Transaction status state machine.
/// Valid transitions for transaction lifecycle (pending → processing → completed/failed, dlq → pending, …).
pub const TRANSACTION_TRANSITIONS: &[Transition] = &[
    // From pending
    Transition {
        from: "pending",
        to: "processing",
    },
    Transition {
        from: "pending",
        to: "completed",
    },
    Transition {
        from: "pending",
        to: "failed",
    },
    // From processing
    Transition {
        from: "processing",
        to: "completed",
    },
    Transition {
        from: "processing",
        to: "failed",
    },
    // From failed (reprocess)
    Transition {
        from: "failed",
        to: "pending",
    },
    // From dlq (requeue)
    Transition {
        from: "dlq",
        to: "pending",
    },
];

/// Settlement status state machine.
/// Valid transitions for settlement lifecycle (completed → pending_review → disputed → adjusted → …).
pub const SETTLEMENT_TRANSITIONS: &[Transition] = &[
    Transition {
        from: "completed",
        to: "pending_review",
    },
    Transition {
        from: "pending_review",
        to: "disputed",
    },
    Transition {
        from: "pending_review",
        to: "voided",
    },
    Transition {
        from: "pending_review",
        to: "completed",
    },
    Transition {
        from: "disputed",
        to: "adjusted",
    },
    Transition {
        from: "disputed",
        to: "voided",
    },
    Transition {
        from: "adjusted",
        to: "completed",
    },
];

/// Validates a transition within a given set of allowed transitions.
/// Allows same-state transitions (idempotent).
pub fn is_valid_transition(from: &str, to: &str, allowed: &[Transition]) -> bool {
    if from == to {
        return true;
    }
    allowed.iter().any(|t| t.from == from && t.to == to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_transaction_transitions_coverage() {
        let transitions: HashSet<_> = TRANSACTION_TRANSITIONS.iter().cloned().collect();
        assert_eq!(
            transitions.len(),
            TRANSACTION_TRANSITIONS.len(),
            "no duplicate transitions"
        );

        // Verify expected pairs exist
        assert!(transitions.contains(&Transition {
            from: "pending",
            to: "processing"
        }));
        assert!(transitions.contains(&Transition {
            from: "failed",
            to: "pending"
        }));
        assert!(transitions.contains(&Transition {
            from: "dlq",
            to: "pending"
        }));
    }

    #[test]
    fn test_settlement_transitions_coverage() {
        let transitions: HashSet<_> = SETTLEMENT_TRANSITIONS.iter().cloned().collect();
        assert_eq!(
            transitions.len(),
            SETTLEMENT_TRANSITIONS.len(),
            "no duplicate transitions"
        );

        assert!(transitions.contains(&Transition {
            from: "completed",
            to: "pending_review"
        }));
        assert!(transitions.contains(&Transition {
            from: "disputed",
            to: "adjusted"
        }));
    }

    #[test]
    fn test_same_state_always_valid() {
        assert!(is_valid_transition(
            "pending",
            "pending",
            TRANSACTION_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "completed",
            "completed",
            SETTLEMENT_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "arbitrary_state",
            "arbitrary_state",
            &[]
        ));
    }

    #[test]
    fn test_invalid_transaction_transitions_rejected() {
        assert!(!is_valid_transition(
            "completed",
            "pending",
            TRANSACTION_TRANSITIONS
        ));
        assert!(!is_valid_transition(
            "processing",
            "pending",
            TRANSACTION_TRANSITIONS
        ));
    }

    #[test]
    fn test_invalid_settlement_transitions_rejected() {
        // Same-state transitions are always valid (idempotent) — see
        // `test_same_state_always_valid` — so only cross-state rejections belong here.
        assert!(!is_valid_transition(
            "adjusted",
            "disputed",
            SETTLEMENT_TRANSITIONS
        ));
    }
}
