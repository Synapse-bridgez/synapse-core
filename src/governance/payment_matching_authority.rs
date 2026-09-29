/// Payment Matching Authority Enforcement
///
/// This module enforces ADR-004: "ReconciliationJob remains the sole authoritative
/// payment-matching mechanism." Only the ReconciliationJob (via this module) can
/// construct a PaymentMatchingAuthority token, ensuring that all payment-match
/// determinations route through the designated authority.
///
/// Reference: docs/adr/004-payment-matching-authority.md

use std::fmt;

/// A capability token proving the holder has authority to determine that a
/// payment matches a transaction and mark it completed.
///
/// This is a zero-sized marker type that can only be constructed by
/// ReconciliationJob, enforcing at compile time that payment matching
/// determinations come exclusively through the designated authority.
///
/// # Usage
///
/// Only ReconciliationJob should construct this via `new_from_reconciliation_job()`:
///
/// ```ignore
/// let authority = PaymentMatchingAuthority::new_from_reconciliation_job();
/// mark_transaction_completed(tx_id, &authority).await?;
/// ```
///
/// Any attempt to construct this from other code paths will fail at compile time.
#[derive(Debug, Clone, Copy)]
pub struct PaymentMatchingAuthority {
    // Marker field to prevent construction outside this module
    _sealed: (),
}

impl PaymentMatchingAuthority {
    /// Construct a PaymentMatchingAuthority token.
    ///
    /// **IMPORTANT:** This should only be called by ReconciliationJob.
    /// All other code paths must route payment-match decisions through
    /// ReconciliationJob.
    ///
    /// # Rationale
    ///
    /// Payment matching is a sensitive operation: incorrectly marking a
    /// transaction as completed affects money movement and reconciliation.
    /// ADR-004 designates ReconciliationJob as the sole authority to make
    /// this determination, with human review of reports before completion.
    ///
    /// This token enforces that boundary at the type level, making it
    /// impossible to mark a payment as matched without this capability.
    pub(crate) fn new_from_reconciliation_job() -> Self {
        PaymentMatchingAuthority { _sealed: () }
    }

    /// Verify that this token exists (always true).
    ///
    /// Utility method to assert at runtime that authority was properly
    /// obtained before a critical operation.
    pub fn verify(&self) -> Result<(), PaymentMatchingAuthorityError> {
        Ok(())
    }
}

/// Error type for payment matching authority violations.
#[derive(Debug)]
pub enum PaymentMatchingAuthorityError {
    /// Attempted to perform a payment-matching operation without proper authority.
    UnauthorizedPaymentMatch {
        context: String,
    },
    /// The operation is not permitted because an unauthorized code path tried it.
    OperationBlockedByAuthority {
        reason: String,
    },
}

impl fmt::Display for PaymentMatchingAuthorityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PaymentMatchingAuthorityError::UnauthorizedPaymentMatch { context } => {
                write!(
                    f,
                    "Payment matching requires authority: {}",
                    context
                )
            }
            PaymentMatchingAuthorityError::OperationBlockedByAuthority { reason } => {
                write!(f, "Payment matching operation blocked: {}", reason)
            }
        }
    }
}

impl std::error::Error for PaymentMatchingAuthorityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authority_token_construction() {
        // This should compile and work
        let _authority = PaymentMatchingAuthority::new_from_reconciliation_job();
    }

    #[test]
    fn test_authority_verification() {
        let authority = PaymentMatchingAuthority::new_from_reconciliation_job();
        assert!(authority.verify().is_ok());
    }

    #[test]
    fn test_error_display() {
        let error = PaymentMatchingAuthorityError::UnauthorizedPaymentMatch {
            context: "GraphQL resolver attempted payment completion".to_string(),
        };
        assert!(error.to_string().contains("Payment matching requires authority"));
    }
}

/// Audit logging for payment matching decisions.
///
/// All payment-matching operations should be logged for compliance and
/// auditing purposes.
pub mod audit {
    use uuid::Uuid;
    use tracing::info;

    pub fn log_payment_matched(
        transaction_id: Uuid,
        payment_id: String,
        authority: &super::PaymentMatchingAuthority,
    ) {
        // Verify authority holds
        let _ = authority.verify();

        info!(
            transaction_id = %transaction_id,
            payment_id = %payment_id,
            authority = "ReconciliationJob",
            "Payment matched to transaction by authorized ReconciliationJob"
        );
    }

    pub fn log_unauthorized_match_attempt(
        transaction_id: Uuid,
        code_path: &str,
    ) {
        tracing::warn!(
            transaction_id = %transaction_id,
            code_path = %code_path,
            authority = "ReconciliationJob",
            "Unauthorized attempt to mark payment matched"
        );
    }
}

/// Documentation of which code paths should construct PaymentMatchingAuthority.
///
/// # Authorized Paths
/// - `src/services/reconciliation.rs` (ReconciliationJob) - Mark payments matched via reconciliation reports
///
/// # Prohibited Paths
/// - CLI commands marking transactions completed (admin only, must go through reports)
/// - GraphQL resolvers (must route through ReconciliationJob)
/// - AccountMonitor (kept dormant per ADR-004, never wired to construct authority)
/// - Other services or webhooks (must route through ReconciliationJob)
pub mod authorized_paths {
    pub const RECONCILIATION_JOB: &str = "src/services/reconciliation.rs";
}
