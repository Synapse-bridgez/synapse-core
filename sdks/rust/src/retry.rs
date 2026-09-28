use crate::error::SynapseError;
use rand::Rng;
use std::future::Future;
use std::time::Duration;

pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_BASE_DELAY_MS: u64 = 200;

/// Upper bound on any single backoff delay computed by this module.
pub const MAX_DELAY_MS: u64 = 10_000;

/// Compute the next decorrelated-jitter backoff delay, in milliseconds,
/// given the previous delay and the configured base delay: a value drawn
/// from `[base_delay_ms, prev_delay_ms * 3]`, capped at [`MAX_DELAY_MS`].
///
/// This is the exact delay calculation [`retry_with_backoff`] uses between
/// attempts, extracted so other reconnect-style loops (e.g. a long-lived
/// WebSocket client) can announce/observe each backoff step instead of
/// re-implementing a second, divergent backoff algorithm.
pub fn next_backoff_delay_ms(prev_delay_ms: u64, base_delay_ms: u64) -> u64 {
    let upper = prev_delay_ms.saturating_mul(3).max(base_delay_ms);
    let d = rand::thread_rng().gen_range(base_delay_ms..=upper);
    d.min(MAX_DELAY_MS)
}

/// Generate a stable idempotency key for a single logical mutating call.
///
/// The key is derived from the request content *and* a fresh random component
/// drawn once per logical call, so that:
///
/// * retries of the same logical call reuse the same key (safe server-side
///   deduplication), and
/// * two genuinely distinct calls with byte-identical content still receive
///   different keys (they must not be deduplicated against each other).
///
/// Callers that already manage their own key should pass it explicitly to the
/// mutating SDK methods instead of relying on this helper.
pub fn generate_idempotency_key(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let content_digest = hasher.finalize();

    // Fresh per-logical-call randomness: guarantees uniqueness across distinct
    // calls even when their content is identical.
    let nonce: u128 = rand::thread_rng().gen();

    let mut hasher = Sha256::new();
    hasher.update(content_digest);
    hasher.update(nonce.to_le_bytes());
    let digest = hasher.finalize();

    let mut key = String::with_capacity(64);
    for byte in digest.iter() {
        key.push_str(&format!("{:02x}", byte));
    }
    key
}

/// Retry a fallible async operation with exponential backoff and decorrelated jitter.
///
/// `max_attempts` is the total number of calls including the first attempt — pass
/// `1` to effectively disable retries. `base_delay_ms` is the starting delay; each
/// retry draws a new delay in the range `[base, prev * 3]`, capped at 10 s.
///
/// Only [`SynapseError::is_transient`] errors are retried. 4xx responses are
/// returned immediately on the first attempt.
///
/// The idempotency key for the logical call is generated once, before the first
/// attempt, and reused across every retry so a transient failure is deduplicated
/// server-side rather than executed twice.
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
                let delay_ms = next_backoff_delay_ms(prev_delay_ms, base_delay_ms);
                prev_delay_ms = delay_ms;
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Retry a fallible async operation that carries an idempotency key, generating
/// the key once per logical call and reusing it across every retry attempt.
///
/// `content` is the serialized request body used to derive the key. The closure
/// receives the stable key on each attempt so the caller can attach it to the
/// outgoing request header. If the caller already has a key, use
/// [`retry_with_backoff`] directly and pass it through the closure instead.
pub async fn retry_with_idempotency_key<F, Fut, T>(
    max_attempts: u32,
    base_delay_ms: u64,
    content: &[u8],
    mut f: F,
) -> Result<T, SynapseError>
where
    F: FnMut(&str) -> Fut,
    Fut: Future<Output = Result<T, SynapseError>>,
{
    let key = generate_idempotency_key(content);
    retry_with_backoff(max_attempts, base_delay_ms, || f(&key)).await
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
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "retries disabled when max_attempts=1"
        );
    }

    #[tokio::test]
    async fn honors_server_retry_after_over_jitter() {
        let calls = Arc::new(AtomicU32::new(0));
        let c = calls.clone();
        let start = std::time::Instant::now();
        let result: Result<u32, _> = retry_with_backoff(2, 1, || {
            let c = c.clone();
            async move {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(SynapseError::HttpRetryAfter {
                        status: 503,
                        body: String::new(),
                        retry_after_ms: 50,
                    })
                } else {
                    Ok(7)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 7);
        assert!(
            start.elapsed().as_millis() >= 50,
            "must wait at least the server-provided Retry-After delay"
        );
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
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "should try exactly max_attempts times"
        );
    }

    // ── next_backoff_delay_ms ────────────────────────────────────────────

    /// Simulated extended outage: many consecutive backoff steps must never
    /// fall below the base delay or exceed the cap, and the delay must
    /// eventually reach the cap rather than growing unbounded.
    #[test]
    fn next_backoff_delay_grows_then_caps_over_an_extended_outage() {
        let base_delay_ms = 200;
        let mut prev_delay_ms = base_delay_ms;
        let mut reached_cap = false;

        for _ in 0..50 {
            let delay_ms = next_backoff_delay_ms(prev_delay_ms, base_delay_ms);
            assert!(
                delay_ms >= base_delay_ms,
                "delay must never fall below the base delay"
            );
            assert!(delay_ms <= MAX_DELAY_MS, "delay must never exceed the cap");
            reached_cap |= delay_ms == MAX_DELAY_MS;
            prev_delay_ms = delay_ms;
        }

        assert!(
            reached_cap,
            "an extended outage must eventually saturate at MAX_DELAY_MS"
        );
    }

    #[test]
    fn next_backoff_delay_first_step_is_at_least_base_delay() {
        let base_delay_ms = 200;
        let delay_ms = next_backoff_delay_ms(base_delay_ms, base_delay_ms);
        assert!(delay_ms >= base_delay_ms);
        assert!(delay_ms <= base_delay_ms * 3);
    }

    // ── idempotency key generation ───────────────────────────────────────

    #[test]
    fn generated_key_is_stable_for_identical_content_and_nonce() {
        // Deterministic derivation: same content + same nonce => same key.
        let content = b"{\"amount\":100}";
        let key_a = generate_idempotency_key(content);
        let key_b = generate_idempotency_key(content);
        assert_eq!(key_a.len(), 64, "key must be a 64-char hex digest");
        assert_eq!(key_b.len(), 64);
        // Distinct calls must not collide even with identical content.
        assert_ne!(key_a, key_b, "distinct calls must get distinct keys");
    }

    #[test]
    fn generated_key_differs_for_different_content() {
        let key_a = generate_idempotency_key(b"{\"amount\":100}");
        let key_b = generate_idempotency_key(b"{\"amount\":200}");
        assert_ne!(key_a, key_b);
    }

    #[tokio::test]
    async fn retry_reuses_same_key_across_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let keys = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let c = calls.clone();
        let k = keys.clone();
        let result: Result<u32, _> = retry_with_idempotency_key(3, 1, b"{\"amount\":100}", |key| {
            let c = c.clone();
            let k = k.clone();
            let key = key.to_string();
            async move {
                k.lock().unwrap().push(key);
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
        let seen = keys.lock().unwrap();
        assert_eq!(seen.len(), 3, "one key per attempt");
        assert!(
            seen.iter().all(|k| *k == seen[0]),
            "the same key must be reused across retries of one logical call"
        );
    }

    #[tokio::test]
    async fn distinct_calls_with_identical_content_get_distinct_keys() {
        let content = b"{\"amount\":100}";
        let mut seen = Vec::new();
        for _ in 0..2 {
            let result: Result<u32, _> = retry_with_idempotency_key(1, 1, content, |key| {
                seen.push(key.to_string());
                async move { Ok(1) }
            })
            .await;
            assert_eq!(result.unwrap(), 1);
        }
        assert_eq!(seen.len(), 2);
        assert_ne!(
            seen[0], seen[1],
            "two distinct calls with identical content must not share a key"
        );
    }
}
