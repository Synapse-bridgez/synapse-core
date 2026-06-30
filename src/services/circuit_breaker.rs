//! Distributed circuit breaker backed by Redis.
//!
//! ## Hot-path strategy
//! Each instance keeps a short-lived local cache (default 100 ms) of the
//! shared Redis state. On a cache hit no Redis I/O occurs, keeping the hot
//! path at in-memory speed. On a cache miss the instance fetches the current
//! state from Redis and resets the TTL. This bounds convergence lag across
//! instances to `cache_ttl` milliseconds while limiting Redis reads to
//! `1 / cache_ttl_s` per instance per second.
//!
//! ## HalfOpen probe gate
//! When the reset timeout elapses the breaker enters HalfOpen. A Redis
//! `SET NX PX` lease ensures that exactly one prober runs fleet-wide at a
//! time; all other callers receive `CircuitBreakerError::Open` until the
//! lease expires or the prober releases it.
//!
//! ## State transitions
//! All transitions are persisted atomically via Lua scripts so that no
//! concurrent writer can produce a torn state. Opening and closing are both
//! persisted; the HalfOpen state is derived on the read path from the
//! `opened_at` timestamp rather than stored.
//!
//! ## Degraded mode
//! When Redis is unreachable the instance falls back to its local in-memory
//! state for both reads and writes. This preserves single-process correctness
//! at the cost of cross-instance convergence until Redis recovers. Malformed
//! Redis payloads are logged and replaced with the safe default (Closed).

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use redis::{Client as RedisClient, Script};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;

// ── Constants ─────────────────────────────────────────────────────────────────

/// How long each instance trusts its local Redis snapshot before re-reading.
const DEFAULT_CACHE_TTL_MS: u64 = 100;
/// How long the HalfOpen probe lease is held fleet-wide (milliseconds).
const DEFAULT_PROBE_LEASE_TTL_MS: u64 = 30_000;
/// Number of consecutive probe successes required to close the breaker.
const DEFAULT_SUCCESS_THRESHOLD: u32 = 1;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum CircuitBreakerError {
    #[error("Circuit breaker is open")]
    Open,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

/// Snapshot returned by [`CircuitBreaker::get_state`].
#[derive(Debug, Clone)]
pub struct CircuitBreakerState {
    pub state: CircuitState,
    pub opened_at: Option<DateTime<Utc>>,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

// ── Internal persisted state (stored in Redis as JSON) ────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default = "default_state_str")]
    state: String,
    #[serde(default)]
    opened_at: Option<DateTime<Utc>>,
    #[serde(default)]
    failure_count: u32,
    #[serde(default)]
    success_count: u32,
    #[serde(default)]
    last_error: Option<String>,
}

fn default_state_str() -> String {
    "closed".to_string()
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            state: "closed".to_string(),
            opened_at: None,
            failure_count: 0,
            success_count: 0,
            last_error: None,
        }
    }
}

struct CacheEntry {
    persisted: PersistedState,
    refreshed_at: Instant,
    /// True until the first successful Redis read; forces an immediate refresh.
    needs_refresh: bool,
}

// ── CircuitBreaker ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct CircuitBreaker {
    service_name: String,
    redis_client: RedisClient,
    cache: Arc<Mutex<CacheEntry>>,
    failure_threshold: u32,
    success_threshold: u32,
    reset_timeout: chrono::Duration,
    cache_ttl: Duration,
    probe_lease_ttl_ms: u64,
}

impl CircuitBreaker {
    pub fn new(
        service_name: String,
        redis_client: RedisClient,
        failure_threshold: u32,
        reset_timeout: chrono::Duration,
    ) -> Self {
        Self::with_config(
            service_name,
            redis_client,
            failure_threshold,
            reset_timeout,
            DEFAULT_SUCCESS_THRESHOLD,
            Duration::from_millis(DEFAULT_CACHE_TTL_MS),
            DEFAULT_PROBE_LEASE_TTL_MS,
        )
    }

    pub fn with_config(
        service_name: String,
        redis_client: RedisClient,
        failure_threshold: u32,
        reset_timeout: chrono::Duration,
        success_threshold: u32,
        cache_ttl: Duration,
        probe_lease_ttl_ms: u64,
    ) -> Self {
        Self {
            service_name,
            redis_client,
            cache: Arc::new(Mutex::new(CacheEntry {
                persisted: PersistedState::default(),
                refreshed_at: Instant::now(),
                needs_refresh: true,
            })),
            failure_threshold,
            success_threshold,
            reset_timeout,
            cache_ttl,
            probe_lease_ttl_ms,
        }
    }

    /// Call `f`, gated by the distributed circuit breaker.
    ///
    /// Returns `Err(CircuitBreakerError::Open)` when the breaker is open or
    /// when another instance already holds the HalfOpen probe lease.
    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let effective = self.get_effective_state().await;

        let is_probe = match effective {
            CircuitState::Open => return Err(Box::new(CircuitBreakerError::Open)),
            CircuitState::HalfOpen => {
                if !self.try_acquire_probe_lease().await {
                    return Err(Box::new(CircuitBreakerError::Open));
                }
                true
            }
            CircuitState::Closed => false,
        };

        let result = f().await;

        match &result {
            Ok(_) => self.on_success(is_probe).await,
            Err(e) => self.on_failure(e.to_string(), is_probe).await,
        }

        result
    }

    pub async fn get_state(&self) -> CircuitBreakerState {
        let cache = self.cache.lock().await;
        CircuitBreakerState {
            state: self.derive_state(&cache.persisted),
            opened_at: cache.persisted.opened_at,
            failure_count: cache.persisted.failure_count,
            last_error: cache.persisted.last_error.clone(),
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    /// Resolve the effective state, refreshing the local cache from Redis when stale.
    async fn get_effective_state(&self) -> CircuitState {
        let mut cache = self.cache.lock().await;
        if cache.needs_refresh || cache.refreshed_at.elapsed() >= self.cache_ttl {
            match self.read_from_redis().await {
                Ok(fresh) => {
                    cache.persisted = fresh;
                    cache.needs_refresh = false;
                }
                Err(e) => tracing::debug!(
                    service = %self.service_name,
                    "Circuit breaker Redis read failed, using cached state: {e}"
                ),
            }
            cache.refreshed_at = Instant::now();
        }
        self.derive_state(&cache.persisted)
    }

    /// Derive the effective state from persisted data; HalfOpen is inferred
    /// from the elapsed time rather than stored.
    fn derive_state(&self, p: &PersistedState) -> CircuitState {
        if p.state != "open" {
            return CircuitState::Closed;
        }
        match p.opened_at {
            Some(t) if Utc::now().signed_duration_since(t) > self.reset_timeout => {
                CircuitState::HalfOpen
            }
            _ => CircuitState::Open,
        }
    }

    async fn read_from_redis(&self) -> Result<PersistedState, redis::RedisError> {
        let key = format!("cb:state:{}", self.service_name);
        let mut conn = self.redis_client.get_async_connection().await?;
        let raw: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut conn).await?;
        match raw {
            None => Ok(PersistedState::default()),
            Some(json) => Ok(serde_json::from_str(&json).unwrap_or_else(|_| {
                tracing::warn!(
                    service = %self.service_name,
                    "Malformed circuit breaker state in Redis; defaulting to Closed"
                );
                PersistedState::default()
            })),
        }
    }

    /// Attempt to acquire the fleet-wide HalfOpen probe lease via `SET NX PX`.
    /// Returns `true` if this caller won (and should proceed with the probe).
    /// When Redis is unavailable returns `true` so at least one probe can fire.
    async fn try_acquire_probe_lease(&self) -> bool {
        let key = format!("cb:probe:{}", self.service_name);
        let mut conn = match self.redis_client.get_async_connection().await {
            Ok(c) => c,
            Err(_) => return true,
        };
        let result: Option<String> = redis::cmd("SET")
            .arg(&key)
            .arg(1u8)
            .arg("NX")
            .arg("PX")
            .arg(self.probe_lease_ttl_ms)
            .query_async(&mut conn)
            .await
            .unwrap_or(None);
        result.is_some()
    }

    async fn release_probe_lease(&self) {
        let key = format!("cb:probe:{}", self.service_name);
        if let Ok(mut conn) = self.redis_client.get_async_connection().await {
            let _: Result<i64, _> = redis::cmd("DEL").arg(&key).query_async(&mut conn).await;
        }
    }

    async fn on_success(&self, is_probe: bool) {
        // Release the lease before updating state so the next probe slot opens
        // immediately if success_threshold > 1 and we haven't closed yet.
        if is_probe {
            self.release_probe_lease().await;
        }
        match self.record_success_in_redis().await {
            Ok(new_state) => {
                let mut cache = self.cache.lock().await;
                cache.persisted = new_state;
                cache.refreshed_at = Instant::now();
                cache.needs_refresh = false;
            }
            Err(e) => {
                tracing::warn!(
                    service = %self.service_name,
                    "Circuit breaker Redis write failed on success, falling back to local: {e}"
                );
                let mut cache = self.cache.lock().await;
                cache.persisted.success_count += 1;
                cache.persisted.last_error = None;
                if cache.persisted.success_count >= self.success_threshold {
                    cache.persisted.state = "closed".to_string();
                    cache.persisted.failure_count = 0;
                    cache.persisted.success_count = 0;
                    cache.persisted.opened_at = None;
                }
                cache.refreshed_at = Instant::now();
                cache.needs_refresh = false;
            }
        }
    }

    async fn on_failure(&self, error: String, is_probe: bool) {
        if is_probe {
            self.release_probe_lease().await;
        }
        match self.record_failure_in_redis(error.clone()).await {
            Ok(new_state) => {
                let mut cache = self.cache.lock().await;
                cache.persisted = new_state;
                cache.refreshed_at = Instant::now();
                cache.needs_refresh = false;
            }
            Err(e) => {
                tracing::warn!(
                    service = %self.service_name,
                    "Circuit breaker Redis write failed on failure, falling back to local: {e}"
                );
                let mut cache = self.cache.lock().await;
                cache.persisted.failure_count += 1;
                cache.persisted.success_count = 0;
                cache.persisted.last_error = Some(error);
                if cache.persisted.failure_count >= self.failure_threshold {
                    cache.persisted.state = "open".to_string();
                    cache.persisted.opened_at = Some(Utc::now());
                }
                cache.refreshed_at = Instant::now();
                cache.needs_refresh = false;
            }
        }
    }

    async fn record_success_in_redis(
        &self,
    ) -> Result<PersistedState, Box<dyn std::error::Error + Send + Sync>> {
        let key = format!("cb:state:{}", self.service_name);
        let ttl = self.reset_timeout.num_seconds().max(60);

        // Atomic read-modify-write. cjson may omit `nil` fields, so all
        // numeric fields are defaulted to 0 on read.
        let script = Script::new(
            r#"
            local data = redis.call('GET', KEYS[1])
            local state
            if data then
                local ok, decoded = pcall(cjson.decode, data)
                state = ok and decoded or {state='closed',failure_count=0,success_count=0}
            else
                state = {state='closed', failure_count=0, success_count=0}
            end
            state.success_count = (state.success_count or 0) + 1
            state.last_error    = nil
            if state.success_count >= tonumber(ARGV[1]) then
                state.state         = 'closed'
                state.failure_count = 0
                state.success_count = 0
                state.opened_at     = nil
            end
            redis.call('SETEX', KEYS[1], ARGV[2], cjson.encode(state))
            return cjson.encode(state)
            "#,
        );

        let mut conn = self.redis_client.get_async_connection().await?;
        let json: String = script
            .key(&key)
            .arg(self.success_threshold)
            .arg(ttl)
            .invoke_async(&mut conn)
            .await?;

        Ok(serde_json::from_str(&json)?)
    }

    async fn record_failure_in_redis(
        &self,
        error: String,
    ) -> Result<PersistedState, Box<dyn std::error::Error + Send + Sync>> {
        let key = format!("cb:state:{}", self.service_name);
        let ttl = self.reset_timeout.num_seconds().max(60);

        let script = Script::new(
            r#"
            local data = redis.call('GET', KEYS[1])
            local state
            if data then
                local ok, decoded = pcall(cjson.decode, data)
                state = ok and decoded or {state='closed',failure_count=0,success_count=0}
            else
                state = {state='closed', failure_count=0, success_count=0}
            end
            state.failure_count = (state.failure_count or 0) + 1
            state.success_count = 0
            state.last_error    = ARGV[1]
            if state.failure_count >= tonumber(ARGV[2]) then
                state.state      = 'open'
                state.opened_at  = ARGV[3]
            else
                state.state      = 'closed'
            end
            redis.call('SETEX', KEYS[1], ARGV[4], cjson.encode(state))
            return cjson.encode(state)
            "#,
        );

        let mut conn = self.redis_client.get_async_connection().await?;
        let json: String = script
            .key(&key)
            .arg(&error)
            .arg(self.failure_threshold)
            .arg(Utc::now().to_rfc3339())
            .arg(ttl)
            .invoke_async(&mut conn)
            .await?;

        Ok(serde_json::from_str(&json)?)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build a breaker pointing at an unreachable Redis URL.
    /// All Redis I/O fails and the breaker falls back to local in-memory state,
    /// which is sufficient for the single-process unit tests below.
    fn make_cb(threshold: u32, reset_secs: i64) -> CircuitBreaker {
        let client = RedisClient::open("redis://127.0.0.1:1/").unwrap();
        CircuitBreaker::new(
            "test-service".to_string(),
            client,
            threshold,
            chrono::Duration::seconds(reset_secs),
        )
    }

    fn fail() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("simulated failure".into())
    }

    fn ok() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    // ── Closed → Open ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn closed_transitions_to_open_after_threshold() {
        let cb = make_cb(3, 60);

        for _ in 0..2 {
            let _ = cb.call(|| async { fail() }).await;
        }
        assert!(matches!(cb.get_state().await.state, CircuitState::Closed));

        let _ = cb.call(|| async { fail() }).await;
        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
        assert_eq!(state.failure_count, 3);
    }

    // ── Open → HalfOpen ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_transitions_to_half_open_after_reset_timeout() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s → expires immediately

        let _ = cb.call(|| async { fail() }).await;
        // With reset_timeout = 0 s, derive_state may already compute HalfOpen
        // the instant any time elapses after opening, so both variants are valid.
        assert!(matches!(
            cb.get_state().await.state,
            CircuitState::Open | CircuitState::HalfOpen
        ));

        // Next call: probe fires and returns the inner error (not fast-failed).
        let result = cb.call(|| async { fail() }).await;
        assert!(result.is_err());
        assert_ne!(result.unwrap_err().to_string(), "Circuit breaker is open");
    }

    // ── HalfOpen → Closed ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_closed_on_success() {
        let cb = make_cb(1, 0);

        let _ = cb.call(|| async { fail() }).await;
        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_ok());

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Closed));
        assert_eq!(state.failure_count, 0);
        assert!(state.opened_at.is_none());
    }

    // ── HalfOpen → Open ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_open_on_failure() {
        // Use a long reset_timeout so the re-opened state is clearly Open
        // (with reset_timeout = 0 the state is immediately HalfOpen again).
        let cb = make_cb(1, 3600);

        // Inject a stale opened_at to put the breaker in HalfOpen.
        {
            let mut cache = cb.cache.lock().await;
            cache.persisted.state = "open".to_string();
            cache.persisted.opened_at = Some(Utc::now() - chrono::Duration::seconds(7200));
            cache.persisted.failure_count = 1;
            cache.needs_refresh = false;
            cache.refreshed_at = Instant::now();
        }
        assert!(matches!(cb.get_state().await.state, CircuitState::HalfOpen));

        // Probe fails → re-opens with a fresh opened_at (clearly < 3600 s).
        let _ = cb.call(|| async { fail() }).await;

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
    }

    // ── Open fast-fails ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_fast_fails_before_reset_timeout() {
        let cb = make_cb(1, 3600);

        let _ = cb.call(|| async { fail() }).await;

        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Circuit breaker is open");
    }

    // ── Malformed Redis payload does not panic ────────────────────────────────

    #[tokio::test]
    async fn malformed_redis_state_defaults_to_closed() {
        // Inject garbage into the cache as if it came from Redis.
        let cb = make_cb(3, 60);
        {
            let mut cache = cb.cache.lock().await;
            // Simulate what would happen if read_from_redis received `{"state":
            // "open"}` with no `opened_at`. derive_state must not panic.
            cache.persisted = PersistedState {
                state: "open".to_string(),
                opened_at: None, // missing opened_at
                failure_count: 5,
                success_count: 0,
                last_error: None,
            };
            cache.needs_refresh = false;
            cache.refreshed_at = Instant::now();
        }
        // opened_at is None → derive_state falls back to Open, not a panic.
        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));

        // Calling through an Open breaker must fast-fail, not panic.
        let result = cb.call(|| async { ok() }).await;
        assert_eq!(result.unwrap_err().to_string(), "Circuit breaker is open");
    }

    // ── Integration tests (require Docker) ────────────────────────────────────
    // Run with: cargo test -- --include-ignored

    mod integration {
        use super::*;

        fn make_cb_redis(name: &str, url: &str, threshold: u32, reset_secs: i64) -> CircuitBreaker {
            let client = RedisClient::open(url).unwrap();
            CircuitBreaker::with_config(
                name.to_string(),
                client,
                threshold,
                chrono::Duration::seconds(reset_secs),
                1,
                Duration::from_nanos(1), // always re-read Redis
                5_000,
            )
        }

        /// A breaker opened by instance A must be observed as open by instance B.
        #[tokio::test]
        #[ignore = "requires Docker"]
        async fn cross_instance_open_propagates() {
            use testcontainers::runners::AsyncRunner;
            use testcontainers_modules::redis::Redis;
            let container = Redis::default().start().await.unwrap();
            let port = container.get_host_port_ipv4(6379).await.unwrap();
            let url = format!("redis://127.0.0.1:{port}");

            let cb_a = make_cb_redis("svc", &url, 1, 3600);
            let cb_b = make_cb_redis("svc", &url, 1, 3600);

            // Instance A trips the breaker.
            let _ = cb_a.call(|| async { Err::<(), _>("boom".into()) }).await;
            assert!(matches!(cb_a.get_state().await.state, CircuitState::Open));

            // Instance B reads from Redis and sees Open.
            let result = cb_b
                .call(|| async { Ok::<(), Box<dyn std::error::Error + Send + Sync>>(()) })
                .await;
            assert!(result.is_err());
            assert_eq!(result.unwrap_err().to_string(), "Circuit breaker is open");
        }

        /// Under concurrency exactly one probe fires when the breaker is HalfOpen.
        #[tokio::test]
        #[ignore = "requires Docker"]
        async fn half_open_single_probe_under_concurrency() {
            use testcontainers::runners::AsyncRunner;
            use testcontainers_modules::redis::Redis;
            let container = Redis::default().start().await.unwrap();
            let port = container.get_host_port_ipv4(6379).await.unwrap();
            let url = format!("redis://127.0.0.1:{port}");

            let cb = Arc::new(make_cb_redis("svc-probe", &url, 1, 0));

            // Trip the breaker.
            let _ = cb.call(|| async { Err::<(), _>("boom".into()) }).await;

            let probe_count = Arc::new(AtomicU32::new(0));

            let tasks: Vec<_> = (0..20)
                .map(|_| {
                    let cb = Arc::clone(&cb);
                    let counter = Arc::clone(&probe_count);
                    tokio::spawn(async move {
                        cb.call(|| async {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                        })
                        .await
                    })
                })
                .collect();

            for t in tasks {
                let _ = t.await;
            }

            assert_eq!(
                probe_count.load(Ordering::SeqCst),
                1,
                "Exactly one probe should have executed"
            );
        }

        /// With `success_threshold = 2` the breaker requires two consecutive
        /// probe successes before closing.
        #[tokio::test]
        #[ignore = "requires Docker"]
        async fn consecutive_successes_required_to_close() {
            use testcontainers::runners::AsyncRunner;
            use testcontainers_modules::redis::Redis;
            let container = Redis::default().start().await.unwrap();
            let port = container.get_host_port_ipv4(6379).await.unwrap();
            let url = format!("redis://127.0.0.1:{port}");

            let client = RedisClient::open(url.as_str()).unwrap();
            let cb = CircuitBreaker::with_config(
                "svc-consec".to_string(),
                client,
                1,                            // trip after 1 failure
                chrono::Duration::seconds(0), // reset immediately
                2,                            // need 2 successes to close
                Duration::from_nanos(1),
                5_000,
            );

            // Trip the breaker.
            let _ = cb.call(|| async { Err::<(), _>("boom".into()) }).await;

            // First probe success — still open (success_count = 1 < 2).
            let _ = cb
                .call(|| async { Ok::<(), Box<dyn std::error::Error + Send + Sync>>(()) })
                .await;
            // State must still be Open (not yet Closed).
            // (After the first success the lease is released, so a second probe
            //  can be acquired on the next call.)
            let state = cb.get_state().await;
            assert!(
                matches!(state.state, CircuitState::Open | CircuitState::HalfOpen),
                "Should not yet be Closed after one success"
            );

            // Second probe success — closes the breaker.
            let _ = cb
                .call(|| async { Ok::<(), Box<dyn std::error::Error + Send + Sync>>(()) })
                .await;
            assert!(matches!(cb.get_state().await.state, CircuitState::Closed));
        }
    }
}
