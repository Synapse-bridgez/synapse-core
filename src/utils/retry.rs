use rand::Rng;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

/// Hard cap on any single retry delay, regardless of attempt count.
const MAX_DELAY_MS: u64 = 10_000;

/// SQLSTATE codes treated as transient (retryable) DB failures.
///
/// - `40P01` deadlock_detected
/// - `40001` serialization_failure
/// - `08000` connection_exception
/// - `08003` connection_does_not_exist
/// - `08001` sqlclient_unable_to_establish_sqlconnection
/// - `08004` sqlserver_rejected_establishment_of_sqlconnection
/// - `08006` connection_failure
/// - `57P03` cannot_connect_now (e.g. DB still starting up / in recovery)
/// - `53300` too_many_connections
fn is_transient_sqlstate(code: &str) -> bool {
    matches!(
        code,
        "40P01" | "40001" | "08000" | "08003" | "08001" | "08004" | "08006" | "57P03" | "53300"
    )
}

/// Last-resort classification when the driver/server didn't surface a SQLSTATE
/// code. Message text is locale- and version-dependent, so this is only
/// consulted when `db_err.code()` is `None`.
fn is_transient_by_message(message: &str) -> bool {
    let msg = message.to_lowercase();
    msg.contains("deadlock detected")
        || msg.contains("serialization failure")
        || msg.contains("connection reset")
        || msg.contains("could not connect")
}

/// Classifies whether a sqlx error is transient (retryable) or permanent.
///
/// Classification prefers the PostgreSQL SQLSTATE code (`db_err.code()`),
/// which is stable across locales and server versions. Message-substring
/// matching is only a fallback for the rare case where no code is available.
pub fn is_transient_db_error(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Io(_) => true,
        sqlx::Error::PoolTimedOut => true,
        sqlx::Error::PoolClosed => false,
        sqlx::Error::Database(db_err) => match db_err.code() {
            Some(code) => is_transient_sqlstate(code.as_ref()),
            None => is_transient_by_message(db_err.message()),
        },
        _ => false,
    }
}

/// Computes the next decorrelated-jitter delay given the previous delay.
///
/// Implements the "decorrelated jitter" formula (`min(cap, random(base, prev * 3))`):
/// each caller draws its own delay from a real RNG rather than deriving it from
/// the wall clock, so callers that fail at the same instant (e.g. a primary
/// failover) spread their retries out instead of retrying in lockstep.
fn decorrelated_jitter_delay_ms(
    prev_delay_ms: u64,
    base_delay_ms: u64,
    cap_ms: u64,
    rng: &mut impl Rng,
) -> u64 {
    let upper = prev_delay_ms.saturating_mul(3).max(base_delay_ms);
    let next = rng.gen_range(base_delay_ms..=upper);
    next.min(cap_ms)
}

/// Retry a fallible async operation with exponential backoff + decorrelated jitter.
///
/// - `max_retries`: maximum number of retry attempts (not counting the initial try)
/// - `base_delay_ms`: base delay in milliseconds before the first retry
/// - Retries only when `is_transient_db_error` returns true
/// - Tracks `db_retry_total` metric by error type
///
/// # Idempotency requirement
///
/// `f` may be invoked more than once for the same logical call. Only wrap
/// operations that are safe to run multiple times — either naturally
/// idempotent (SELECTs, `ON CONFLICT DO NOTHING`/`UPDATE` upserts) or guarded
/// by an explicit idempotency key (e.g. a stable primary key checked before
/// insert). Wrapping a plain, non-idempotent write (such as an unguarded
/// `INSERT`) risks duplicating it if a transient error occurs *after* the
/// write already committed (e.g. the connection drops while reading the
/// commit acknowledgement).
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
    let mut prev_delay_ms = base_delay_ms;
    loop {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) if attempt < max_retries && is_transient_db_error(&err) => {
                attempt += 1;
                // `rand::thread_rng()` is `!Send`, so it's created and dropped
                // within this synchronous block rather than held across the
                // `.await` points in this loop (which would make the
                // resulting future `!Send`, breaking callers that need to
                // spawn or hold it across threads, e.g. axum handlers).
                let delay_ms = decorrelated_jitter_delay_ms(
                    prev_delay_ms,
                    base_delay_ms,
                    MAX_DELAY_MS,
                    &mut rand::thread_rng(),
                );
                prev_delay_ms = delay_ms;

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
        sqlx::Error::Database(db_err) => match db_err.code().as_deref() {
            Some("40P01") => "deadlock",
            Some("40001") => "serialization_failure",
            Some("08000") | Some("08003") | Some("08001") | Some("08004") | Some("08006")
            | Some("57P03") => "connection_reset",
            Some("53300") => "too_many_connections",
            _ => {
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
        },
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
    fn test_sqlstate_classification_ignores_misleading_message() {
        // A message that doesn't mention any known transient keyword, but
        // carries a recognized deadlock SQLSTATE code, must still be
        // classified as transient: code wins over message text.
        assert!(is_transient_sqlstate("40P01"));
        assert!(is_transient_sqlstate("40001"));
        assert!(is_transient_sqlstate("08006"));
        assert!(!is_transient_sqlstate("23505")); // unique_violation: not transient
        assert!(!is_transient_sqlstate("42601")); // syntax_error: not transient
    }

    #[test]
    fn test_message_fallback_only_used_without_code() {
        // The message fallback is last-resort: it should still recognize the
        // documented phrases when no SQLSTATE code is available.
        assert!(is_transient_by_message(
            "FATAL: could not connect to server"
        ));
        assert!(is_transient_by_message("ERROR: deadlock detected"));
        assert!(!is_transient_by_message(
            "ERROR: unique constraint violated"
        ));
    }

    #[test]
    fn test_jitter_distribution_is_spread_not_clustered() {
        // Decorrelated jitter draws from a real RNG per call, so repeated
        // samples should land across a wide spread of values rather than
        // clustering into a handful of buckets (which is what happened with
        // the old wall-clock-derived jitter under tight, correlated timing).
        let mut rng = rand::thread_rng();
        let base = 100u64;
        let cap = 10_000u64;
        let mut prev = base;
        let mut delays = std::collections::HashSet::new();
        for _ in 0..500 {
            let d = decorrelated_jitter_delay_ms(prev, base, cap, &mut rng);
            assert!(d >= base.min(cap));
            assert!(d <= cap);
            prev = d;
            delays.insert(d);
        }
        assert!(
            delays.len() > 250,
            "expected a wide spread of jitter values, got {} unique values out of 500",
            delays.len()
        );
    }

    #[test]
    fn test_concurrent_callers_are_decorrelated() {
        // Two independent callers that fail at the "same instant" (same
        // starting state) must not produce identical retry-delay sequences —
        // otherwise they would retry in lockstep against the DB they're
        // trying to let recover.
        let mut rng_a = rand::thread_rng();
        let mut rng_b = rand::thread_rng();
        let base = 50u64;
        let cap = 10_000u64;

        let mut prev_a = base;
        let mut prev_b = base;
        let mut seq_a = Vec::new();
        let mut seq_b = Vec::new();
        for _ in 0..10 {
            prev_a = decorrelated_jitter_delay_ms(prev_a, base, cap, &mut rng_a);
            prev_b = decorrelated_jitter_delay_ms(prev_b, base, cap, &mut rng_b);
            seq_a.push(prev_a);
            seq_b.push(prev_b);
        }

        assert_ne!(
            seq_a, seq_b,
            "two independent callers produced identical jitter sequences"
        );
    }
}
