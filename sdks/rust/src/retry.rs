use crate::error::SynapseError;
use rand::Rng;
use std::future::Future;
use std::time::Duration;

pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_BASE_DELAY_MS: u64 = 200;

const MAX_DELAY_MS: u64 = 10_000;

/// Retry a fallible async operation with exponential backoff and decorrelated jitter.
///
/// `max_attempts` is the total number of calls including the first attempt — pass
/// `1` to effectively disable retries. `base_delay_ms` is the starting delay; each
/// retry draws a new delay in the range `[base, prev * 3]`, capped at 10 s.
///
/// Only [`SynapseError::is_transient`] errors are retried. 4xx responses are
/// returned immediately on the first attempt.
pub async fn retry_with_backoff<F, Fut, T>(
    max_attempts: u32,
    base_delay_ms: u64,
    mut f: F,
) -> Result<T, SynapseError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SynapseError>>,
{
    let mut attempt = 0u32;
    let mut prev_delay_ms = base_delay_ms;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt + 1 < max_attempts && e.is_transient() => {
                attempt += 1;
                let delay_ms = {
                    let upper = prev_delay_ms.saturating_mul(3).max(base_delay_ms);
                    let d = rand::thread_rng().gen_range(base_delay_ms..=upper);
                    d.min(MAX_DELAY_MS)
                };
                prev_delay_ms = delay_ms;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    fn http_error(status: u16) -> SynapseError {
        SynapseError::Http {
            status,
            body: String::new(),
        }
    }

    #[tokio::test]
    async fn retries_on_5xx_and_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<u32, _> = retry_with_backoff(3, 1, || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(http_error(500))
                } else {
                    Ok(42)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_4xx() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<u32, _> = retry_with_backoff(3, 1, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(http_error(400))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "4xx must not be retried");
    }

    #[tokio::test]
    async fn disabled_when_max_attempts_is_one() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<u32, _> = retry_with_backoff(1, 1, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(http_error(503))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "retries disabled when max_attempts=1");
    }

    #[tokio::test]
    async fn exhausts_all_attempts_on_persistent_5xx() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let result: Result<u32, _> = retry_with_backoff(3, 1, || {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Err(http_error(502))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 3, "should try exactly max_attempts times");
    }
}
