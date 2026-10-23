use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Classification of a Stellar transaction submission error.
///
/// Used by the swap settlement submission path to decide whether a failed
/// submission should be retried (with bounded backoff) or treated as a
/// terminal outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionErrorClass {
    /// The transaction may still be included; safe to retry after backoff.
    /// Covers Horizon timeouts, connection failures, and `tx_too_late`
    /// (the transaction expired without being included, so a fresh
    /// submission with a new time bound is required).
    Transient,
    /// The sequence number was too low or bad; the allocator must re-sync
    /// against the ledger and re-allocate before retrying.
    SequenceConflict,
    /// The transaction was already included (duplicate submission). This is
    /// a success-equivalent outcome and must not be re-applied.
    AlreadyIncluded,
    /// A permanent error that must not be retried.
    Permanent,
}

/// Classifies a Stellar/Horizon submission error string (typically the
/// `result_codes.transaction` or `extras.result_codes` value, or a Horizon
/// error message) into a [`SubmissionErrorClass`].
///
/// Matching is case-insensitive and tolerant of surrounding text so it works
/// both for raw result codes (`"tx_bad_seq"`) and Horizon error bodies.
pub fn classify_submission_error(error: &str) -> SubmissionErrorClass {
    let msg = error.to_lowercase();

    // Duplicate submission: Horizon reports the tx as already included.
    if msg.contains("tx_duplicate")
        || msg.contains("already included")
        || msg.contains("already_included")
        || msg.contains("duplicate submission")
    {
        return SubmissionErrorClass::AlreadyIncluded;
    }

    // Sequence conflicts: re-sync and re-allocate before retrying.
    if msg.contains("tx_bad_seq") || msg.contains("tx_too_low") {
        return SubmissionErrorClass::SequenceConflict;
    }

    // Transient: the transaction may still land, or the network hiccuped.
    if msg.contains("tx_too_late")
        || msg.contains("timeout")
        || msg.contains("timed out")
        || msg.contains("try again later")
        || msg.contains("connection")
        || msg.contains("temporarily unavailable")
        || msg.contains("rate limit")
        || msg.contains("429")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("504")
    {
        return SubmissionErrorClass::Transient;
    }

    SubmissionErrorClass::Permanent
}

/// Returns `true` when the classified error is safe to retry.
pub fn is_retryable_submission_error(error: &str) -> bool {
    matches!(
        classify_submission_error(error),
        SubmissionErrorClass::Transient | SubmissionErrorClass::SequenceConflict
    )
}

/// Computes a bounded exponential backoff delay (with jitter) for the given
/// zero-based `attempt`. The result is clamped to `max_delay_ms` so a chain
/// of pending submissions cannot stall an account indefinitely.
pub fn submission_backoff_ms(attempt: u32, base_delay_ms: u64, max_delay_ms: u64) -> u64 {
    let exp_delay = base_delay_ms.saturating_mul(1u64 << attempt.min(6));
    apply_jitter(exp_delay).min(max_delay_ms)
}
=======

/// Classifies whether a sqlx error is transient (retryable) or permanent.
pub fn is_transient_db_error(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_) => true,
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::PoolClosed => false,
        sqlx::Error::Database(db_err) => {
            let msg = db_err.message().to_lowercase();
            // Deadlock detected (PostgreSQL code 40P01)
            // Serialization failure (PostgreSQL code 40001)
            // Connection reset / connection failure
            msg.contains("deadlock detected")
                || msg.contains("serialization failure")
                || msg.contains("connection reset")
                || msg.contains("could not connect")
                || db_err.code().is_some_and(|c| {
                    matches!(c.as_ref(), "40P01" | "40001" | "08006" | "08001" | "08004")
                })
        }
        _ => false,
    }
}

/// Applies a jitter band (±25%) around `base_delay`, using wall-clock
/// nanoseconds as the entropy source rather than an RNG crate. Unit-agnostic
/// (works for milliseconds, seconds, whatever unit `base_delay` is in) — the
/// caller is responsible for any max-delay clamping appropriate to their
/// unit. Shared by [`retry_with_backoff`]'s DB retry delay and
/// `webhook_dispatcher`'s failure backoff / circuit-breaker reschedule
/// delays, so a shared "reset" moment doesn't turn into a shared "fire this
/// instant" moment for every caller.
pub fn apply_jitter(base_delay: u64) -> u64 {
    let jitter = (base_delay / 4).max(1);
    let jitter_offset = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64)
        % (jitter * 2);
    base_delay.saturating_sub(jitter) + jitter_offset
}

/// Retry a fallible async operation with exponential backoff + jitter.
///
/// - `max_retries`: maximum number of retry attempts (not counting the initial try)
/// - `base_delay_ms`: base delay in milliseconds before the first retry
/// - Retries only when `is_transient_db_error` returns true
/// - Tracks `db_retry_total` metric by error type
pub async fn retry_with_backoff<F, Fut, T>(
    operation_name: &str,
    max_retries: u32,
    base_delay_ms: u64,
    mut f: F,
) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) if attempt < max_retries && is_transient_db_error(&err) => {
                attempt += 1;
                // Exponential backoff: base * 2^(attempt-1), capped at 10s
                let exp_delay = base_delay_ms * (1u64 << (attempt - 1).min(6));
                let delay_ms = apply_jitter(exp_delay).min(10_000);

                warn!(
                    operation = operation_name,
                    attempt,
                    delay_ms,
                    error = %err,
                    "Transient DB error, retrying"
                );

                // Emit retry metric
                tracing::info!(
                    counter.db_retry_total = 1u64,
                    operation = operation_name,
                    error_kind = classify_error_kind(&err),
                    attempt,
                );

                sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(err) => {
                debug!(
                    operation = operation_name,
                    attempt,
                    error = %err,
                    "DB error is permanent or max retries exceeded"
                );
                return Err(err);
            }
        }
    }
}

fn classify_error_kind(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Io(_) => "io",
        sqlx::Error::PoolTimedOut => "pool_timeout",
        sqlx::Error::Database(db_err) => {
            let msg = db_err.message().to_lowercase();
            if msg.contains("deadlock") {
                "deadlock"
            } else if msg.contains("serialization") {
                "serialization_failure"
            } else if msg.contains("connection reset") || msg.contains("could not connect") {
                "connection_reset"
            } else {
                "database"
            }
        }
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_succeeds_after_transient_error() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        // Simulate: first call fails with pool timeout, second succeeds
        let result = retry_with_backoff("test_op", 3, 1, || {
            let cc = cc.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(sqlx::Error::PoolTimedOut)
                } else {
                    Ok(42u32)
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_permanent_error_not_retried() {
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result: Result<u32, sqlx::Error> = retry_with_backoff("test_op", 3, 1, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                // RowNotFound is a permanent error
                Err(sqlx::Error::RowNotFound)
            }
        })
        .await;

        assert!(result.is_err());
        // Should only be called once — no retries for permanent errors
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_is_transient_db_error_pool_timeout() {
        assert!(is_transient_db_error(&sqlx::Error::PoolTimedOut));
    }

    #[test]
    fn test_is_transient_db_error_row_not_found() {
        assert!(!is_transient_db_error(&sqlx::Error::RowNotFound));
    }

    #[test]
    fn test_is_transient_db_error_pool_closed() {
        assert!(!is_transient_db_error(&sqlx::Error::PoolClosed));
    }

    #[test]
    fn test_classify_submission_error_sequence_conflicts() {
        assert_eq!(
            classify_submission_error("tx_bad_seq"),
            SubmissionErrorClass::SequenceConflict
        );
        assert_eq!(
            classify_submission_error("tx_too_low"),
            SubmissionErrorClass::SequenceConflict
        );
        assert!(is_retryable_submission_error("tx_bad_seq"));
    }

    #[test]
    fn test_classify_submission_error_transient() {
        assert_eq!(
            classify_submission_error("tx_too_late"),
            SubmissionErrorClass::Transient
        );
        assert_eq!(
            classify_submission_error("Horizon request timed out"),
            SubmissionErrorClass::Transient
        );
        assert_eq!(
            classify_submission_error("503 Service Unavailable"),
            SubmissionErrorClass::Transient
        );
        assert!(is_retryable_submission_error("timeout"));
    }

    #[test]
    fn test_classify_submission_error_duplicate() {
        assert_eq!(
            classify_submission_error("tx_duplicate"),
            SubmissionErrorClass::AlreadyIncluded
        );
        assert_eq!(
            classify_submission_error("Transaction already included in ledger"),
            SubmissionErrorClass::AlreadyIncluded
        );
        // Duplicate is success-equivalent, not a retryable failure.
        assert!(!is_retryable_submission_error("tx_duplicate"));
    }

    #[test]
    fn test_classify_submission_error_permanent() {
        assert_eq!(
            classify_submission_error("tx_insufficient_balance"),
            SubmissionErrorClass::Permanent
        );
        assert_eq!(
            classify_submission_error("tx_bad_auth"),
            SubmissionErrorClass::Permanent
        );
        assert!(!is_retryable_submission_error("tx_bad_auth"));
    }

    #[test]
    fn test_submission_backoff_is_bounded() {
        let max = 5_000u64;
        for attempt in 0..20 {
            let delay = submission_backoff_ms(attempt, 100, max);
            assert!(delay <= max, "attempt {attempt} produced {delay} > {max}");
        }
        // Backoff should grow (or at least not shrink) before clamping.
        assert!(submission_backoff_ms(3, 100, max) >= submission_backoff_ms(0, 100, max));
    }
}
