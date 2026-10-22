//! Transaction retry and deadlock handling for swap persistence (#1522).
//!
//! Swap writes (orders, quotes, fills, and their outbox events) run in short,
//! highly concurrent transactions. PostgreSQL aborts some of them with
//! serialization failures or deadlocks; those are safe to re-run from the top
//! because the aborted transaction left no trace. [`run_in_transaction`]
//! re-runs the caller's closure in a **fresh** transaction for retryable
//! SQLSTATEs only, with bounded attempts, full-jitter backoff, a cap on total
//! retry time, and an optional caller deadline.
//!
//! Side effects: the closure must do all of its work - including the outbox
//! insert - against the transaction it is handed. Nothing commits until the
//! closure returns `Ok`, so a retried attempt can never duplicate an event.
//! Do not perform non-transactional side effects (HTTP calls, Redis writes)
//! inside the closure.

use futures::future::BoxFuture;
use rand::Rng;
use sqlx::{PgPool, Postgres, Transaction};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// `serialization_failure`
pub const SQLSTATE_SERIALIZATION_FAILURE: &str = "40001";
/// `deadlock_detected`
pub const SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";
/// `lock_not_available` (raised when `lock_timeout` expires or `NOWAIT` fails)
pub const SQLSTATE_LOCK_NOT_AVAILABLE: &str = "55P03";

static RETRIES_TOTAL: AtomicU64 = AtomicU64::new(0);
static EXHAUSTED_TOTAL: AtomicU64 = AtomicU64::new(0);
static DEADLOCKS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Process-wide retry counters, for metrics export and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryStats {
    pub retries: u64,
    pub exhausted: u64,
    pub deadlocks: u64,
}

pub fn retry_stats() -> RetryStats {
    RetryStats {
        retries: RETRIES_TOTAL.load(Ordering::Relaxed),
        exhausted: EXHAUSTED_TOTAL.load(Ordering::Relaxed),
        deadlocks: DEADLOCKS_TOTAL.load(Ordering::Relaxed),
    }
}

/// Whether a database error may be retried by re-running the whole transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorClass {
    /// Transient concurrency failure; the SQLSTATE is carried for logging.
    Retryable(String),
    NonRetryable,
}

/// Classify a database error. Only concurrency aborts are retryable: constraint
/// violations, statement timeouts (`57014`), pool exhaustion and I/O errors are
/// not - re-running them would either fail identically or extend an outage.
pub fn classify(err: &sqlx::Error) -> ErrorClass {
    match err {
        sqlx::Error::Database(db) => match db.code().as_deref() {
            Some(
                code @ (SQLSTATE_SERIALIZATION_FAILURE
                | SQLSTATE_DEADLOCK_DETECTED
                | SQLSTATE_LOCK_NOT_AVAILABLE),
            ) => ErrorClass::Retryable(code.to_string()),
            _ => ErrorClass::NonRetryable,
        },
        _ => ErrorClass::NonRetryable,
    }
}

/// Bounded retry policy.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts including the first. `1` disables retries.
    pub max_attempts: u32,
    /// Backoff ceiling for the first retry; doubles per attempt.
    pub base_delay: Duration,
    /// Upper bound on any single backoff.
    pub max_delay: Duration,
    /// Upper bound on wall-clock time spent across all attempts. Prevents a
    /// long-running transaction from being retried repeatedly.
    pub max_total: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(500),
            max_total: Duration::from_secs(2),
        }
    }
}

impl RetryPolicy {
    /// Full-jitter backoff: uniform in `[0, min(max_delay, base * 2^(attempt-1))]`.
    /// Jitter spreads concurrent retriers apart and prevents retry storms.
    pub fn backoff(&self, attempt: u32) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1u32 << attempt.saturating_sub(1).min(16));
        let ceiling = exp.min(self.max_delay).as_millis() as u64;
        Duration::from_millis(rand::thread_rng().gen_range(0..=ceiling))
    }
}

/// Generic retry loop. Re-runs `attempt` while `classify` reports the error as
/// retryable, the attempt budget is not spent, and the next backoff would not
/// overrun `max_total` or the caller's `deadline`. Returns the last error on
/// exhaustion. Cancellation is preserved: dropping the returned future stops
/// the loop at its next await point.
pub async fn retry_with<T, E, F, Fut, C>(
    policy: &RetryPolicy,
    op: &str,
    deadline: Option<Instant>,
    classify: C,
    mut attempt: F,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    C: Fn(&E) -> ErrorClass,
    E: std::fmt::Display,
{
    let started = Instant::now();
    let mut n = 1;
    loop {
        let err = match attempt().await {
            Ok(v) => return Ok(v),
            Err(e) => e,
        };
        let ErrorClass::Retryable(sqlstate) = classify(&err) else {
            return Err(err);
        };
        if sqlstate == SQLSTATE_DEADLOCK_DETECTED {
            DEADLOCKS_TOTAL.fetch_add(1, Ordering::Relaxed);
            // Per-operation deadlock logs make hotspots visible by `op`.
            tracing::warn!(
                counter.swap_db_deadlocks_total = 1u64,
                op,
                attempt = n,
                "swap persistence: deadlock detected"
            );
        }

        let delay = policy.backoff(n);
        let over_budget = started.elapsed() + delay > policy.max_total;
        let over_deadline = deadline.is_some_and(|d| Instant::now() + delay >= d);
        if n >= policy.max_attempts || over_budget || over_deadline {
            EXHAUSTED_TOTAL.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                counter.swap_db_retry_exhausted_total = 1u64,
                op,
                attempts = n,
                sqlstate = %sqlstate,
                over_budget,
                over_deadline,
                error = %err,
                "swap persistence: retries exhausted"
            );
            return Err(err);
        }

        RETRIES_TOTAL.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            counter.swap_db_retries_total = 1u64,
            op,
            attempt = n,
            sqlstate = %sqlstate,
            delay_ms = delay.as_millis() as u64,
            "swap persistence: retrying transaction"
        );
        tokio::time::sleep(delay).await;
        n += 1;
    }
}

/// Run `f` inside a new transaction, committing on success and retrying the
/// whole transaction on retryable errors (including errors raised at commit).
///
/// ```ignore
/// run_in_transaction(&pool, &RetryPolicy::default(), "fill.record", None, |tx| {
///     Box::pin(async move {
///         sqlx::query("INSERT INTO swap_fills ...").execute(&mut **tx).await?;
///         sqlx::query("INSERT INTO outbox ...").execute(&mut **tx).await?;
///         Ok(())
///     })
/// })
/// .await?;
/// ```
pub async fn run_in_transaction<T, F>(
    pool: &PgPool,
    policy: &RetryPolicy,
    op: &str,
    deadline: Option<Instant>,
    f: F,
) -> Result<T, sqlx::Error>
where
    F: for<'t> Fn(&'t mut Transaction<'static, Postgres>) -> BoxFuture<'t, Result<T, sqlx::Error>>,
{
    let f = &f;
    retry_with(policy, op, deadline, classify, move || async move {
        let attempt = async {
            let mut tx = pool.begin().await?;
            let value = f(&mut tx).await?;
            tx.commit().await?;
            Ok(value)
        };
        // Bound each attempt by the deadline so a hung statement cannot outlive
        // the request; the dropped transaction rolls back.
        match deadline {
            Some(d) => tokio::time::timeout_at(d.into(), attempt)
                .await
                .unwrap_or(Err(sqlx::Error::PoolTimedOut)),
            None => attempt.await,
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[derive(Debug)]
    struct FakeErr(&'static str);

    impl std::fmt::Display for FakeErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }

    fn fake_classify(e: &FakeErr) -> ErrorClass {
        match e.0 {
            SQLSTATE_SERIALIZATION_FAILURE | SQLSTATE_DEADLOCK_DETECTED => {
                ErrorClass::Retryable(e.0.to_string())
            }
            _ => ErrorClass::NonRetryable,
        }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
            max_total: Duration::from_secs(1),
        }
    }

    #[tokio::test]
    async fn retries_serialization_failure_then_succeeds() {
        let calls = AtomicU32::new(0);
        let res: Result<u32, FakeErr> = retry_with(&fast(), "t", None, fake_classify, || async {
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err(FakeErr(SQLSTATE_SERIALIZATION_FAILURE)),
                1 => Err(FakeErr(SQLSTATE_DEADLOCK_DETECTED)),
                n => Ok(n),
            }
        })
        .await;
        assert_eq!(res.unwrap(), 2);
        // The committed side effect happens once: only the successful attempt returns.
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn non_retryable_errors_fail_immediately() {
        let calls = AtomicU32::new(0);
        let res: Result<(), FakeErr> = retry_with(&fast(), "t", None, fake_classify, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(FakeErr("23505"))
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn attempts_are_bounded() {
        let calls = AtomicU32::new(0);
        let res: Result<(), FakeErr> = retry_with(&fast(), "t", None, fake_classify, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(FakeErr(SQLSTATE_DEADLOCK_DETECTED))
        })
        .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn expired_deadline_stops_retrying() {
        let calls = AtomicU32::new(0);
        let deadline = Some(Instant::now());
        let res: Result<(), FakeErr> =
            retry_with(&fast(), "t", deadline, fake_classify, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(FakeErr(SQLSTATE_SERIALIZATION_FAILURE))
            })
            .await;
        assert!(res.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backoff_respects_max_delay() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(100),
            max_total: Duration::from_secs(5),
        };
        for attempt in 1..10 {
            assert!(p.backoff(attempt) <= Duration::from_millis(100));
        }
    }

    #[test]
    fn non_database_errors_are_not_retryable() {
        assert_eq!(
            classify(&sqlx::Error::PoolTimedOut),
            ErrorClass::NonRetryable
        );
        assert_eq!(
            classify(&sqlx::Error::RowNotFound),
            ErrorClass::NonRetryable
        );
    }
}
