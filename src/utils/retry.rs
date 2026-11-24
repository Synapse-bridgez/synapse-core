use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

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

/// Maximum delay between HTTP retries, also the cap applied to a server's
/// `Retry-After` hint. Same ceiling as the DB retry delay above.
pub const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Parses a `Retry-After` header value: either a number of seconds or an HTTP
/// date (RFC 7231 / RFC 2822 form, e.g. `Wed, 21 Oct 2015 07:28:00 GMT`).
/// A date in the past yields a zero delay. Returns `None` for anything else.
pub fn parse_retry_after(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    let value = value.trim();
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let delta = when.with_timezone(&chrono::Utc) - now;
    Some(delta.to_std().unwrap_or(Duration::ZERO))
}

/// How long to sleep before the next HTTP retry: the server's `Retry-After`
/// hint when it sent one, otherwise the computed backoff, in both cases never
/// more than `max`.
pub fn delay_before_retry(
    computed_backoff: Duration,
    retry_after: Option<Duration>,
    max: Duration,
) -> Duration {
    retry_after.unwrap_or(computed_backoff).min(max)
}

/// A failed HTTP attempt, as seen by [`retry_http_with_backoff`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("HTTP request failed with status {status}: {message}")]
pub struct HttpRetryError {
    pub status: u16,
    /// The parsed `Retry-After` header, if the response carried one.
    pub retry_after: Option<Duration>,
    pub message: String,
}

impl HttpRetryError {
    /// 429 and 5xx are retryable; other statuses are not.
    pub fn is_retryable(&self) -> bool {
        self.status == 429 || self.status >= 500
    }
}

/// Retries an HTTP operation with exponential backoff + jitter, honouring the
/// server's `Retry-After` on 429 (and any other retryable response that sends
/// one), capped at [`MAX_BACKOFF`].
pub async fn retry_http_with_backoff<F, Fut, T>(
    operation_name: &str,
    max_retries: u32,
    base_delay_ms: u64,
    mut f: F,
) -> Result<T, HttpRetryError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, HttpRetryError>>,
{
    let mut attempt = 0u32;
    loop {
        match f().await {
            Ok(val) => return Ok(val),
            Err(err) if attempt < max_retries && err.is_retryable() => {
                attempt += 1;
                let exp_delay = base_delay_ms * (1u64 << (attempt - 1).min(6));
                let computed = Duration::from_millis(apply_jitter(exp_delay));
                let delay = delay_before_retry(computed, err.retry_after, MAX_BACKOFF);
                warn!(
                    operation = operation_name,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    status = err.status,
                    honoured_retry_after = err.retry_after.is_some(),
                    "Retryable HTTP error, retrying"
                );
                sleep(delay).await;
            }
            Err(err) => return Err(err),
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
}
