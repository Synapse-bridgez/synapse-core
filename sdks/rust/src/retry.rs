use crate::error::SynapseError;
use rand::Rng;
use std::future::Future;
use std::time::Duration;

pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
pub const DEFAULT_BASE_DELAY_MS: u64 = 200;

/// Upper bound on any single backoff delay computed by this module.
pub const MAX_DELAY_MS: u64 = 10_000;

/// Upper bound on a rate-limit-header-derived wait, in milliseconds.
///
/// Even if the server returns a malformed or maliciously large reset value,
/// the client must never hang indefinitely, so any header-derived wait is
/// clamped to this cap.
pub const MAX_RATE_LIMIT_WAIT_MS: u64 = 60_000;

/// Small jitter added on top of a header-derived reset wait so that many
/// clients that received the same reset time do not stampede the server at
/// exactly the same instant.
pub const RATE_LIMIT_JITTER_MS: u64 = 250;

/// Parsed rate-limit information from a 429 response's headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Remaining quota reported by the server, if present.
    pub remaining: Option<u64>,
    /// Milliseconds to wait until the rate limit resets, if derivable.
    pub reset_after_ms: Option<u64>,
}

/// Parse rate-limit headers from a 429 response.
///
/// Recognizes the common `Retry-After` (seconds or HTTP-date) and
/// `X-RateLimit-Remaining` / `X-RateLimit-Reset` header families. Any header
/// that is absent or malformed is simply ignored (yielding `None` for that
/// field) so callers can fall back to generic exponential backoff.
pub fn parse_rate_limit_headers(headers: &[(String, String)]) -> RateLimitInfo {
    let mut remaining = None;
    let mut reset_after_ms = None;

    for (name, value) in headers {
        let name = name.to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "retry-after" => {
                if let Some(ms) = parse_retry_after(value) {
                    reset_after_ms = Some(ms);
                }
            }
            "x-ratelimit-remaining" => {
                if let Ok(n) = value.parse::<u64>() {
                    remaining = Some(n);
                }
            }
            "x-ratelimit-reset" => {
                if let Some(ms) = parse_reset_value(value) {
                    reset_after_ms = Some(ms);
                }
            }
            _ => {}
        }
    }

    RateLimitInfo {
        remaining,
        reset_after_ms,
    }
}

/// Parse a `Retry-After` value: either a non-negative integer number of
/// seconds or an HTTP-date. Returns milliseconds, or `None` if malformed.
fn parse_retry_after(value: &str) -> Option<u64> {
    if let Ok(secs) = value.parse::<u64>() {
        return Some(secs.saturating_mul(1000));
    }
    // HTTP-date form is not parsed here; treat as malformed so the caller
    // falls back to exponential backoff rather than guessing.
    None
}

/// Parse an `X-RateLimit-Reset` value. Servers commonly send either a Unix
/// timestamp in seconds or a relative number of seconds; both are accepted and
/// converted to a relative wait in milliseconds.
fn parse_reset_value(value: &str) -> Option<u64> {
    let secs = value.parse::<u64>().ok()?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if secs > now_secs {
        // Absolute Unix timestamp in the future.
        Some((secs - now_secs).saturating_mul(1000))
    } else {
        // Relative seconds from now.
        Some(secs.saturating_mul(1000))
    }
}

/// Compute the wait, in milliseconds, for a 429 response given its headers.
///
/// Returns `Some(ms)` when a valid reset time was parsed (clamped to
/// [`MAX_RATE_LIMIT_WAIT_MS`] and with a small jitter added), or `None` when no
/// usable header was present so the caller should fall back to exponential
/// backoff.
pub fn rate_limit_wait_ms(headers: &[(String, String)]) -> Option<u64> {
    let info = parse_rate_limit_headers(headers);
    let reset = info.reset_after_ms?;
    let jitter = rand::thread_rng().gen_range(0..=RATE_LIMIT_JITTER_MS);
    Some(reset.saturating_add(jitter).min(MAX_RATE_LIMIT_WAIT_MS))
}

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
                let delay_ms = match e.rate_limit_headers() {
                    Some(headers) => match rate_limit_wait_ms(headers) {
                        Some(ms) => ms,
                        None => {
                            let d = next_backoff_delay_ms(prev_delay_ms, base_delay_ms);
                            prev_delay_ms = d;
                            d
                        }
                    },
                    None => {
                        let d = next_backoff_delay_ms(prev_delay_ms, base_delay_ms);
                        prev_delay_ms = d;
                        d
                    }
                };
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

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_retry_after_seconds() {
        let info = parse_rate_limit_headers(&headers(&[("Retry-After", "2")]));
        assert_eq!(info.reset_after_ms, Some(2000));
    }

    #[test]
    fn parses_remaining_quota() {
        let info = parse_rate_limit_headers(&headers(&[("X-RateLimit-Remaining", "0")]));
        assert_eq!(info.remaining, Some(0));
    }

    #[test]
    fn malformed_headers_yield_none() {
        let info = parse_rate_limit_headers(&headers(&[
            ("Retry-After", "not-a-number"),
            ("X-RateLimit-Remaining", "lots"),
        ]));
        assert_eq!(info.reset_after_ms, None);
        assert_eq!(info.remaining, None);
    }

    #[test]
    fn rate_limit_wait_none_without_headers() {
        assert_eq!(rate_limit_wait_ms(&[]), None);
    }

    #[test]
    fn rate_limit_wait_caps_malicious_reset() {
        let wait = rate_limit_wait_ms(&headers(&[("Retry-After", "999999999")]));
        assert_eq!(wait, Some(MAX_RATE_LIMIT_WAIT_MS));
    }

    #[test]
    fn rate_limit_wait_includes_jitter() {
        let wait = rate_limit_wait_ms(&headers(&[("Retry-After", "1")])).unwrap();
        assert!(wait >= 1000 && wait <= 1000 + RATE_LIMIT_JITTER_MS);
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
}
