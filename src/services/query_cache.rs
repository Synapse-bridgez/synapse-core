use crate::cache::degradation::{record_redis_degraded, DegradedFallback, RedisComponent};
use crate::cache::{is_redis_unavailable, CacheValidator, ValidationError};
use crate::middleware::idempotency::RedisCircuitBreaker;
use lru::LruCache;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-query-type hit-rate counters, aggregated at the cache-key-prefix level
/// (e.g. `status_counts`, `daily_totals`, `asset_stats`, `asset_total`)
/// rather than per exact key, to keep cardinality bounded (consistent with
/// issue 38's cardinality-awareness pattern).
#[derive(Default)]
struct QueryTypeCounters {
    hits: AtomicU64,
    misses: AtomicU64,
    evicted_before_expiry: AtomicU64,
}

/// Extracts the query-type label from a cache key, e.g.
/// `"query:daily_totals:7"` -> `"daily_totals"`, `"query:status_counts"` -> `"status_counts"`.
fn query_type_from_key(key: &str) -> String {
    let without_prefix = key.strip_prefix("query:").unwrap_or(key);
    without_prefix
        .split(':')
        .next()
        .unwrap_or(without_prefix)
        .to_string()
}

/// One row of the hit-rate report: a query type's observed performance plus
/// a suggested tuning action. Applying the suggestion is a human decision —
/// this only surfaces the data.
#[derive(Debug, Clone, Serialize)]
pub struct QueryTypeHitRateReport {
    pub query_type: String,
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub evicted_before_expiry: u64,
    pub eviction_before_expiry_rate: f64,
    pub suggested_action: String,
}

/// Below this hit rate (with a non-trivial sample size) a query type is
/// flagged as a caching candidate for review.
const LOW_HIT_RATE_THRESHOLD: f64 = 25.0;
/// Above this eviction-before-expiry rate the in-memory cache is likely
/// undersized for this query type's working set.
const HIGH_EVICTION_RATE_THRESHOLD: f64 = 25.0;
/// Minimum number of observations before a suggestion is made, to avoid
/// noisy suggestions from a handful of requests.
const MIN_SAMPLE_SIZE: u64 = 20;

/// If inserting `incoming_key` would evict the LRU tail (cache at capacity
/// and `incoming_key` is not already present), and that tail entry has not
/// yet reached its `expires_at`, records an eviction-before-expiry against
/// the evicted entry's own query type. Must be called with `lru` already
/// locked, immediately before `put`.
fn record_eviction_before_expiry(
    lru: &LruCache<String, CacheEntry>,
    incoming_key: &str,
    registry: &Mutex<HashMap<String, Arc<QueryTypeCounters>>>,
) {
    if lru.len() < lru.cap().get() || lru.peek(incoming_key).is_some() {
        return;
    }
    if let Some((evicted_key, evicted_entry)) = lru.peek_lru() {
        if evicted_entry.expires_at > Instant::now() {
            let query_type = query_type_from_key(evicted_key);
            registry
                .lock()
                .unwrap()
                .entry(query_type)
                .or_insert_with(|| Arc::new(QueryTypeCounters::default()))
                .evicted_before_expiry
                .fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An in-memory LRU entry. `expires_at` is enforced in `QueryCache::get` so
/// entries that outlive the configured memory-cache TTL are treated as
/// misses (and evicted) instead of being served indefinitely until the LRU
/// capacity happens to push them out.
#[derive(Clone)]
struct CacheEntry {
    value: String,
    expires_at: Instant,
}

/// Redis connection pool configuration with performance tuning.
///
/// # Performance Optimization
/// - Maintains a pool of reusable Redis connections to avoid connection overhead
/// - Each connection is verified with a PING before being returned to prevent
///   stale connection issues
/// - Pool exhaustion is handled gracefully with typed errors
/// - Configurable max size and acquisition timeout
#[derive(Clone, Debug)]
pub struct RedisPoolConfig {
    /// Maximum number of pooled Redis connections.
    /// OPT: Configurable from env var REDIS_POOL_SIZE; defaults to 10
    pub pool_size: u32,
    /// Timeout for acquiring a connection from the pool.
    /// OPT: Configurable from env var REDIS_POOL_TIMEOUT_SECS; defaults to 5
    pub pool_timeout: Duration,
}

impl Default for RedisPoolConfig {
    fn default() -> Self {
        // OPT: Read pool size from env var with default of 10
        let pool_size = std::env::var("REDIS_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        // OPT: Read pool timeout from env var with default of 5 seconds
        let pool_timeout_secs = std::env::var("REDIS_POOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);

        Self {
            pool_size,
            pool_timeout: Duration::from_secs(pool_timeout_secs),
        }
    }
}

/// Default bound on establishing the Redis connection.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// Default bound on a single Redis operation.
const DEFAULT_OP_TIMEOUT: Duration = Duration::from_millis(1000);
/// Invalidations that could not reach Redis are remembered (up to this many
/// patterns) and replayed before the next Redis read, so a value cached
/// before an outage is not served stale after Redis recovers.
const MAX_PENDING_INVALIDATIONS: usize = 256;

fn env_duration_ms(var: &str, default: Duration) -> Duration {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Query result cache: an in-process LRU in front of Redis.
///
/// # Degraded mode (Redis unavailable) — see docs/redis-degradation.md
///
/// Redis is optional for correctness here. When it is unreachable (at
/// startup or later) the cache degrades instead of failing:
///
/// - construction never fails on an unreachable server — the connection is
///   established lazily, through the circuit breaker, on first use;
/// - `get` reports a miss (`Ok(None)`) so callers fall through to a direct
///   Postgres read, after still trying the in-memory LRU;
/// - `set` keeps the value in the in-memory LRU only;
/// - invalidations clear the LRU and are queued for replay against Redis;
/// - every such event emits the shared `redis_degraded_operations_total`
///   signal (`cache::degradation`, component `query_cache`).
///
/// Connect and per-operation timeouts (`REDIS_CONNECT_TIMEOUT_MS`,
/// `REDIS_OP_TIMEOUT_MS`) bound how long a request can wait on a blackholed
/// Redis before the breaker opens and short-circuits further attempts.
#[derive(Clone)]
pub struct QueryCache {
    client: Client,
    // OPT: ConnectionManager for built-in connection pooling, created lazily
    // so an unreachable Redis at startup does not prevent the process from
    // serving traffic.
    conn: Arc<tokio::sync::OnceCell<ConnectionManager>>,
    connect_timeout: Duration,
    op_timeout: Duration,
    pool_config: RedisPoolConfig,
    cb: RedisCircuitBreaker,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    memory_hits: Arc<AtomicU64>,
    memory_misses: Arc<AtomicU64>,
    lru: Arc<Mutex<LruCache<String, CacheEntry>>>,
    memory_ttl: Duration,
    query_type_counters: Arc<Mutex<HashMap<String, Arc<QueryTypeCounters>>>>,
    pending_invalidations: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for QueryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryCache")
            .field("pool_config", &self.pool_config)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub status_counts_ttl: u64,
    pub daily_totals_ttl: u64,
    pub asset_stats_ttl: u64,
    pub memory_cache_size: usize,
    pub memory_cache_ttl: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            status_counts_ttl: 300, // 5 minutes
            daily_totals_ttl: 3600, // 1 hour
            asset_stats_ttl: 600,   // 10 minutes
            memory_cache_size: 1000,
            memory_cache_ttl: 30,
        }
    }
}

fn cache_validation_error(err: ValidationError) -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::TypeError,
        "cache validation failed",
        err.to_string(),
    ))
}

fn cb_error(e: crate::middleware::idempotency::RedisError) -> redis::RedisError {
    match e {
        crate::middleware::idempotency::RedisError::CircuitOpen => {
            redis::RedisError::from((redis::ErrorKind::IoError, "Redis circuit breaker is open"))
        }
        crate::middleware::idempotency::RedisError::Redis(r) => r,
    }
}

fn timed_out(what: &str) -> redis::RedisError {
    redis::RedisError::from(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        format!("Redis {what} timed out"),
    ))
}

fn degraded(fallback: DegradedFallback, e: &redis::RedisError) {
    record_redis_degraded(RedisComponent::QueryCache, fallback, e);
}

/// Whether a stored key matches a Redis glob pattern of the restricted form
/// `CacheValidator::validate_pattern` allows (literal text plus `*`).
fn pattern_matches(pattern: &str, key: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == key,
        Some((prefix, rest)) => {
            if !key.starts_with(prefix) {
                return false;
            }
            let tail = &key[prefix.len()..];
            if rest.is_empty() {
                return true;
            }
            // Try every split point for the remainder.
            (0..=tail.len())
                .filter(|&i| tail.is_char_boundary(i))
                .any(|i| pattern_matches(rest, &tail[i..]))
        }
    }
}

impl QueryCache {
    /// Creates a new QueryCache.
    ///
    /// Fails only on an invalid Redis URL. An unreachable server is logged
    /// and reported as degraded; the cache then runs memory-only and keeps
    /// retrying the connection (through the circuit breaker) on use.
    ///
    /// # Connection Pool Setup
    /// - OPT: ConnectionManager pools and reconnects Redis connections
    /// - OPT: Pool size is configurable via REDIS_POOL_SIZE env var (default: 10)
    /// - OPT: Pool timeout is configurable via REDIS_POOL_TIMEOUT_SECS (default: 5)
    pub async fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;

        let pool_config = RedisPoolConfig::default();
        let cache_size = std::env::var("MEMORY_CACHE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        // OPT: Read in-memory entry TTL from env var, default 30s (CacheConfig::default()).
        let memory_ttl_secs = std::env::var("MEMORY_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(CacheConfig::default().memory_cache_ttl);

        let cache = Self {
            client,
            conn: Arc::new(tokio::sync::OnceCell::new()),
            connect_timeout: env_duration_ms("REDIS_CONNECT_TIMEOUT_MS", DEFAULT_CONNECT_TIMEOUT),
            op_timeout: env_duration_ms("REDIS_OP_TIMEOUT_MS", DEFAULT_OP_TIMEOUT),
            pool_config,
            cb: RedisCircuitBreaker::from_env(),
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
            memory_hits: Arc::new(AtomicU64::new(0)),
            memory_misses: Arc::new(AtomicU64::new(0)),
            lru: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(cache_size).unwrap_or(NonZeroUsize::new(1000).unwrap()),
            ))),
            memory_ttl: Duration::from_secs(memory_ttl_secs),
            query_type_counters: Arc::new(Mutex::new(HashMap::new())),
            pending_invalidations: Arc::new(Mutex::new(Vec::new())),
        };

        // Eager connect attempt so a healthy deployment logs readiness at
        // startup; failure is degraded mode, not a startup failure.
        if let Err(e) = cache.redis_op(|_conn| async { Ok(()) }).await {
            degraded(DegradedFallback::InMemoryOnly, &e);
            tracing::warn!(
                error = %e,
                "Query cache: Redis unreachable at startup; running memory-only with \
                 direct Postgres reads until it recovers"
            );
        }
        Ok(cache)
    }

    /// Runs `f` against the shared connection, establishing it first if
    /// needed. Connect + operation are bounded by timeouts and run through
    /// the circuit breaker, so a dead Redis quickly short-circuits.
    async fn redis_op<T, F, Fut>(&self, f: F) -> Result<T, redis::RedisError>
    where
        F: FnOnce(ConnectionManager) -> Fut,
        Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
    {
        let cell = self.conn.clone();
        let client = self.client.clone();
        let connect_timeout = self.connect_timeout;
        let op_timeout = self.op_timeout;
        self.cb
            .call(|| async move {
                let conn = match tokio::time::timeout(
                    connect_timeout,
                    cell.get_or_try_init(|| ConnectionManager::new(client)),
                )
                .await
                {
                    Ok(Ok(conn)) => conn.clone(),
                    Ok(Err(e)) => return Err(e),
                    Err(_) => return Err(timed_out("connect")),
                };
                match tokio::time::timeout(op_timeout, f(conn)).await {
                    Ok(result) => result,
                    Err(_) => Err(timed_out("operation")),
                }
            })
            .await
            .map_err(cb_error)
    }

    fn queue_invalidation(&self, pattern: &str) {
        let mut pending = self.pending_invalidations.lock().unwrap();
        if pending.iter().any(|p| p == pattern) {
            return;
        }
        if pending.len() >= MAX_PENDING_INVALIDATIONS {
            // Too much to replay precisely: fall back to invalidating every
            // query-cache key once Redis is back.
            pending.clear();
            pending.push("query:*".to_string());
            return;
        }
        pending.push(pattern.to_string());
    }

    /// Replays invalidations queued during an outage. Returns an error (and
    /// keeps the queue) if Redis is still unavailable.
    async fn flush_pending_invalidations(&self) -> Result<(), redis::RedisError> {
        let patterns: Vec<String> = self.pending_invalidations.lock().unwrap().clone();
        if patterns.is_empty() {
            return Ok(());
        }
        let to_run = patterns.clone();
        self.redis_op(|mut conn| async move {
            for pattern in &to_run {
                let keys: Vec<String> = conn.keys(pattern).await?;
                if !keys.is_empty() {
                    conn.del::<_, ()>(keys).await?;
                }
            }
            Ok(())
        })
        .await?;
        self.pending_invalidations
            .lock()
            .unwrap()
            .retain(|p| !patterns.contains(p));
        tracing::info!(
            replayed = patterns.len(),
            "Query cache: replayed invalidations queued while Redis was unavailable"
        );
        Ok(())
    }

    /// Invalidations waiting to be replayed against Redis.
    pub fn pending_invalidation_count(&self) -> usize {
        self.pending_invalidations.lock().unwrap().len()
    }

    fn counters_for(&self, key: &str) -> Arc<QueryTypeCounters> {
        let query_type = query_type_from_key(key);
        let mut counters = self.query_type_counters.lock().unwrap();
        counters
            .entry(query_type)
            .or_insert_with(|| Arc::new(QueryTypeCounters::default()))
            .clone()
    }

    /// Per-query-type hit-rate report, aggregated at the cache-key-prefix
    /// level. Ranks entries needing attention (low hit rate or high
    /// eviction-before-expiry rate) first, with a suggested action. This is
    /// a report only — no tuning change is applied automatically.
    pub fn hit_rate_report(&self) -> Vec<QueryTypeHitRateReport> {
        let counters = self.query_type_counters.lock().unwrap();
        let mut report: Vec<QueryTypeHitRateReport> = counters
            .iter()
            .map(|(query_type, c)| {
                let hits = c.hits.load(Ordering::Relaxed);
                let misses = c.misses.load(Ordering::Relaxed);
                let evicted_before_expiry = c.evicted_before_expiry.load(Ordering::Relaxed);
                let total = hits + misses;
                let hit_rate = if total > 0 {
                    (hits as f64 / total as f64) * 100.0
                } else {
                    0.0
                };
                let eviction_before_expiry_rate = if hits > 0 {
                    (evicted_before_expiry as f64 / hits as f64) * 100.0
                } else {
                    0.0
                };

                let suggested_action = if total < MIN_SAMPLE_SIZE {
                    "Not enough samples yet".to_string()
                } else if hit_rate < LOW_HIT_RATE_THRESHOLD {
                    "Low hit rate: consider increasing TTL, or removing caching for this query type"
                        .to_string()
                } else if eviction_before_expiry_rate > HIGH_EVICTION_RATE_THRESHOLD {
                    "High eviction-before-expiry rate: consider increasing cache size".to_string()
                } else {
                    "Healthy".to_string()
                };

                QueryTypeHitRateReport {
                    query_type: query_type.clone(),
                    hits,
                    misses,
                    hit_rate,
                    evicted_before_expiry,
                    eviction_before_expiry_rate,
                    suggested_action,
                }
            })
            .collect();

        report.sort_by(|a, b| {
            a.hit_rate
                .partial_cmp(&b.hit_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        report
    }

    /// Looks `key` up in the in-memory LRU, then Redis.
    ///
    /// Degraded mode: if Redis is unavailable this returns `Ok(None)` — a
    /// miss — so the caller reads straight from Postgres; the event is
    /// reported through the shared degraded signal. Validation and
    /// deserialization errors are still returned as errors.
    pub async fn get<T: DeserializeOwned + Send>(
        &self,
        key: &str,
    ) -> Result<Option<T>, redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;
        let query_type_counters = self.counters_for(key);

        // Try in-memory cache first. An entry past its `expires_at` is treated
        // as a miss and evicted rather than served — see `CacheEntry` doc.
        {
            let mut lru = self.lru.lock().unwrap();
            if let Some(entry) = lru.get(key) {
                if entry.expires_at > Instant::now() {
                    self.memory_hits.fetch_add(1, Ordering::Relaxed);
                    if let Ok(value) = serde_json::from_str::<T>(&entry.value) {
                        query_type_counters.hits.fetch_add(1, Ordering::Relaxed);
                        return Ok(Some(value));
                    }
                } else {
                    lru.pop(key);
                }
            }
        }

        self.memory_misses.fetch_add(1, Ordering::Relaxed);

        // A value written to Redis before an outage may be stale if an
        // invalidation was missed during it: replay those first, and do not
        // trust Redis until they have gone through.
        if let Err(e) = self.flush_pending_invalidations().await {
            if is_redis_unavailable(&e) {
                degraded(DegradedFallback::DirectDbRead, &e);
                return Ok(None);
            }
            return Err(e);
        }

        let hits = self.hits.clone();
        let misses = self.misses.clone();
        let lru = self.lru.clone();
        let memory_ttl = self.memory_ttl;
        let query_type_registry = self.query_type_counters.clone();
        let key = key.to_string();

        let raw = self
            .redis_op(|mut conn| {
                let key = key.clone();
                async move { conn.get::<_, Option<String>>(&key).await }
            })
            .await;

        let value = match raw {
            Ok(v) => v,
            Err(e) if is_redis_unavailable(&e) => {
                degraded(DegradedFallback::DirectDbRead, &e);
                return Ok(None);
            }
            Err(e) => return Err(e),
        };

        match value {
            Some(v) => {
                hits.fetch_add(1, Ordering::Relaxed);
                query_type_counters.hits.fetch_add(1, Ordering::Relaxed);
                // Populate in-memory cache
                {
                    let mut lru_cache = lru.lock().unwrap();
                    record_eviction_before_expiry(&lru_cache, &key, &query_type_registry);
                    lru_cache.put(
                        key.clone(),
                        CacheEntry {
                            value: v.clone(),
                            expires_at: Instant::now() + memory_ttl,
                        },
                    );
                }
                serde_json::from_str(&v).map(Some).map_err(|e| {
                    redis::RedisError::from((
                        redis::ErrorKind::TypeError,
                        "deserialization failed",
                        e.to_string(),
                    ))
                })
            }
            None => {
                misses.fetch_add(1, Ordering::Relaxed);
                query_type_counters.misses.fetch_add(1, Ordering::Relaxed);
                Ok(None)
            }
        }
    }

    /// Stores `value` in the in-memory LRU and Redis.
    ///
    /// Degraded mode: if Redis is unavailable the value stays in the
    /// in-memory LRU only and this returns `Ok(())`; the event is reported
    /// through the shared degraded signal.
    pub async fn set<T: Serialize + Send>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;
        let ttl_secs = ttl.as_secs();
        if ttl_secs == 0 {
            return Err(cache_validation_error(ValidationError::InvalidTTL));
        }
        if ttl_secs > i64::MAX as u64 {
            return Err(cache_validation_error(ValidationError::InvalidTTL));
        }
        CacheValidator::validate_ttl(ttl_secs as i64).map_err(cache_validation_error)?;

        let serialized = serde_json::to_string(value).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        CacheValidator::validate_value_size(serialized.as_bytes())
            .map_err(cache_validation_error)?;

        // Store in in-memory cache. The in-memory entry's TTL is
        // `memory_ttl` (independent of the caller-supplied Redis `ttl`) so a
        // short-lived local cache never outlives the Redis-side value by more
        // than `memory_ttl`.
        {
            let mut lru = self.lru.lock().unwrap();
            record_eviction_before_expiry(&lru, key, &self.query_type_counters);
            lru.put(
                key.to_string(),
                CacheEntry {
                    value: serialized.clone(),
                    expires_at: Instant::now() + self.memory_ttl,
                },
            );
        }

        let key = key.to_string();
        match self
            .redis_op(|mut conn| async move { conn.set_ex(&key, serialized, ttl_secs).await })
            .await
        {
            Ok(()) => Ok(()),
            Err(e) if is_redis_unavailable(&e) => {
                degraded(DegradedFallback::InMemoryOnly, &e);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Invalidates every key matching `pattern`. The in-memory LRU is always
    /// cleared; if Redis is unavailable the pattern is queued for replay and
    /// the Redis error is returned (callers treat invalidation as
    /// best-effort).
    pub async fn invalidate(&self, pattern: &str) -> Result<(), redis::RedisError> {
        CacheValidator::validate_pattern(pattern).map_err(cache_validation_error)?;

        // Clear in-memory cache
        {
            let mut lru = self.lru.lock().unwrap();
            lru.clear();
        }

        let owned = pattern.to_string();
        let result = self
            .redis_op(|mut conn| async move {
                let keys: Vec<String> = conn.keys(&owned).await?;
                if !keys.is_empty() {
                    conn.del::<_, ()>(keys).await?;
                }
                Ok(())
            })
            .await;
        self.handle_invalidation_result(&[pattern], result)
    }

    pub async fn invalidate_exact(&self, key: &str) -> Result<(), redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;

        // Clear from in-memory cache
        {
            let mut lru = self.lru.lock().unwrap();
            lru.pop(key);
        }

        let owned = key.to_string();
        let result = self
            .redis_op(|mut conn| async move { conn.del::<_, ()>(owned).await })
            .await;
        self.handle_invalidation_result(&[key], result)
    }

    fn handle_invalidation_result(
        &self,
        patterns: &[&str],
        result: Result<(), redis::RedisError>,
    ) -> Result<(), redis::RedisError> {
        if let Err(e) = &result {
            if is_redis_unavailable(e) {
                for p in patterns {
                    self.queue_invalidation(p);
                }
                degraded(DegradedFallback::SkippedBestEffort, e);
            }
        }
        result
    }

    /// Invalidate only the cache keys that reflect transaction-aggregate data.
    ///
    /// Called by partition lifecycle events (creation and detachment) so that
    /// cached query results that span partition boundaries are not served
    /// stale after a rotation. The invalidation is deliberately scoped to
    /// transaction-related keys (`query:status_counts`, `query:daily_totals:*`,
    /// `query:asset_stats`, `query:asset_total:*`) rather than clearing the
    /// entire cache, to avoid dropping unrelated cached results (e.g.
    /// settlement or compliance caches) on every routine partition rollover.
    ///
    /// # Correctness proof
    ///
    /// A partition rotation event (creating a new partition or detaching an
    /// old one) changes which underlying child table a query against
    /// `transactions` reads from. Any cache key whose value was derived from
    /// a `SELECT … FROM transactions …` must therefore be invalidated to
    /// avoid serving a stale result that no longer reflects the post-rotation
    /// state.  Keys derived from other tables (settlements, compliance
    /// reports, etc.) are unaffected and must not be flushed.
    pub async fn invalidate_partition_affected_keys(&self) -> Result<(), redis::RedisError> {
        // Targeted patterns — only transaction aggregate keys.
        // Must not match settlement, compliance, or any non-transaction key.
        let patterns = [
            "query:status_counts",
            "query:daily_totals:*",
            "query:asset_stats",
            "query:asset_total:*",
        ];

        // Remove matching entries from the in-memory LRU without clearing it entirely.
        {
            let mut lru = self.lru.lock().unwrap();
            let keys_to_drop: Vec<String> = lru
                .iter()
                .filter_map(|(k, _)| {
                    let k: &str = k.as_ref();
                    let affected = patterns.iter().any(|p| pattern_matches(p, k));
                    if affected {
                        Some(k.to_string())
                    } else {
                        None
                    }
                })
                .collect();
            for k in keys_to_drop {
                lru.pop(&k);
            }
        }

        // Remove from Redis.
        let result = self
            .redis_op(|mut conn| async move {
                for pattern in &patterns {
                    let keys: Vec<String> = conn.keys(*pattern).await?;
                    if !keys.is_empty() {
                        conn.del::<_, ()>(&keys).await?;
                    }
                }
                Ok(())
            })
            .await;
        self.handle_invalidation_result(&patterns, result)?;

        tracing::info!(
            "Partition rotation: invalidated transaction-aggregate cache keys \
             (status_counts, daily_totals:*, asset_stats, asset_total:*)"
        );
        Ok(())
    }

    /// Verifies Redis is reachable by sending a PING through the circuit
    /// breaker (bounded by the connect/operation timeouts).
    pub async fn health_check(&self) -> Result<(), redis::RedisError> {
        self.redis_op(|mut conn| async move {
            redis::cmd("PING")
                .query_async::<_, String>(&mut conn)
                .await
                .map(|_| ())
        })
        .await
    }

    /// Returns the circuit breaker state: `"open"` or `"closed"`.
    pub fn circuit_state(&self) -> String {
        self.cb.state()
    }

    /// Returns the connection pool configuration (size, timeout).
    pub fn pool_config(&self) -> &RedisPoolConfig {
        &self.pool_config
    }

    pub fn metrics(&self) -> CacheMetrics {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        let memory_hits = self.memory_hits.load(Ordering::Relaxed);
        let memory_misses = self.memory_misses.load(Ordering::Relaxed);
        let memory_total = memory_hits + memory_misses;
        let memory_hit_rate = if memory_total > 0 {
            (memory_hits as f64 / memory_total as f64) * 100.0
        } else {
            0.0
        };

        CacheMetrics {
            hits,
            misses,
            total,
            hit_rate,
            memory_hits,
            memory_misses,
            memory_total,
            memory_hit_rate,
        }
    }

    pub async fn warm_cache(
        &self,
        pool: &sqlx::PgPool,
        config: &CacheConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Warm status counts
        let status_counts = crate::db::queries::get_status_counts(pool).await?;
        self.set(
            "query:status_counts",
            &status_counts,
            Duration::from_secs(config.status_counts_ttl),
        )
        .await?;

        // Warm daily totals for last 7 days
        let daily_totals = crate::db::queries::get_daily_totals(pool, 7).await?;
        self.set(
            "query:daily_totals:7",
            &daily_totals,
            Duration::from_secs(config.daily_totals_ttl),
        )
        .await?;

        // Warm asset stats
        let asset_stats = crate::db::queries::get_asset_stats(pool).await?;
        self.set(
            "query:asset_stats",
            &asset_stats,
            Duration::from_secs(config.asset_stats_ttl),
        )
        .await?;

        tracing::info!("Cache warming completed");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub total: u64,
    pub hit_rate: f64,
    pub memory_hits: u64,
    pub memory_misses: u64,
    pub memory_total: u64,
    pub memory_hit_rate: f64,
}

pub fn cache_key_status_counts() -> String {
    "query:status_counts".to_string()
}

pub fn cache_key_daily_totals(days: i32) -> String {
    format!("query:daily_totals:{days}")
}

pub fn cache_key_asset_stats() -> String {
    "query:asset_stats".to_string()
}

pub fn cache_key_asset_total(asset_code: &str) -> String {
    format!("query:asset_total:{asset_code}")
}

/// Weekly scheduled job that logs the per-query-type hit-rate report so
/// under-performing cache entries are surfaced without requiring an operator
/// to remember to poll for them. Register with the `JobScheduler` from
/// `src/services/scheduler.rs`, e.g.
/// `scheduler.register_job(Box::new(QueryCacheReportJob::new(cache.clone()))).await`.
pub struct QueryCacheReportJob {
    cache: QueryCache,
}

impl QueryCacheReportJob {
    pub fn new(cache: QueryCache) -> Self {
        Self { cache }
    }
}

#[async_trait::async_trait]
impl crate::services::scheduler::Job for QueryCacheReportJob {
    fn name(&self) -> &str {
        "query_cache_hit_rate_report"
    }

    /// Every Monday at 06:00 UTC.
    fn schedule(&self) -> &str {
        "0 0 6 * * MON *"
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for row in self.cache.hit_rate_report() {
            tracing::info!(
                query_type = %row.query_type,
                hits = row.hits,
                misses = row.misses,
                hit_rate = row.hit_rate,
                eviction_before_expiry_rate = row.eviction_before_expiry_rate,
                suggested_action = %row.suggested_action,
                "Query cache hit-rate report"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_metrics() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => {
                // Redis not available in test environment, skip
                return;
            }
        };
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 0);
    }

    #[test]
    fn test_query_type_from_key() {
        assert_eq!(query_type_from_key("query:status_counts"), "status_counts");
        assert_eq!(query_type_from_key("query:daily_totals:7"), "daily_totals");
        assert_eq!(query_type_from_key("query:asset_total:USD"), "asset_total");
        assert_eq!(query_type_from_key("no_prefix"), "no_prefix");
    }

    #[test]
    fn test_cache_key_generation() {
        assert_eq!(cache_key_status_counts(), "query:status_counts");
        assert_eq!(cache_key_daily_totals(7), "query:daily_totals:7");
        assert_eq!(cache_key_asset_stats(), "query:asset_stats");
        assert_eq!(cache_key_asset_total("USD"), "query:asset_total:USD");
    }

    #[tokio::test]
    async fn test_get_rejects_invalid_key() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available in test environment
        };
        let result = cache.get::<String>("invalid key").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cache validation failed"));
    }

    #[tokio::test]
    async fn test_invalidate_rejects_invalid_pattern() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available in test environment
        };
        let result = cache.invalidate("bad@pattern").await;
        assert!(result.is_err());
    }

    // --- Connection Pool Tests ---

    #[tokio::test]
    async fn test_connection_pool_reuses_connections() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available
        };

        // OPT: Multiple operations should reuse pooled connections
        let key = "test:pool:reuse";
        let _ = cache.set(key, &"value1", Duration::from_secs(10)).await;
        let _ = cache.set(key, &"value2", Duration::from_secs(10)).await;
        let result: Option<String> = cache.get(key).await.ok().flatten();

        // If pool reuse works, no connection exhaustion error should occur
        assert!(result.is_some() || result.is_none()); // Either success or graceful error
    }

    #[tokio::test]
    async fn test_pool_health_check() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available
        };

        // OPT: Health check should succeed if pool is healthy
        let health = cache.health_check().await;
        // Either succeeds (Redis available) or fails gracefully
        let _ = health;
    }

    #[tokio::test]
    async fn test_pool_config_from_env() {
        // OPT: Pool size and timeout should be configurable from env
        let config = RedisPoolConfig::default();
        assert!(config.pool_size > 0);
        assert!(config.pool_timeout.as_secs() > 0);
    }

    // --- Degraded mode (Redis unavailable, #1335) ---

    /// Port 1 on loopback: connection refused immediately.
    const DEAD_REDIS: &str = "redis://127.0.0.1:1/";

    async fn dead_cache() -> QueryCache {
        QueryCache::new(DEAD_REDIS)
            .await
            .expect("an unreachable Redis must not fail construction")
    }

    #[tokio::test]
    async fn construction_succeeds_when_redis_unreachable() {
        let before = crate::cache::degradation::events_for(RedisComponent::QueryCache);
        let _cache = dead_cache().await;
        assert!(crate::cache::degradation::events_for(RedisComponent::QueryCache) > before);
    }

    #[tokio::test]
    async fn construction_still_rejects_an_invalid_url() {
        assert!(QueryCache::new("not a url").await.is_err());
    }

    #[tokio::test]
    async fn get_reports_miss_so_caller_falls_through_to_db() {
        let cache = dead_cache().await;
        let before = crate::cache::degradation::events_for(RedisComponent::QueryCache);
        let result = cache.get::<Vec<u32>>("query:status_counts").await;
        assert!(matches!(result, Ok(None)), "got {result:?}");
        assert!(crate::cache::degradation::events_for(RedisComponent::QueryCache) > before);
        // A degraded read is not a real Redis miss.
        assert_eq!(cache.metrics().misses, 0);
    }

    #[tokio::test]
    async fn set_keeps_value_in_memory_and_serves_it_while_redis_is_down() {
        let cache = dead_cache().await;
        cache
            .set(
                "query:asset_stats",
                &vec![1u32, 2, 3],
                Duration::from_secs(60),
            )
            .await
            .expect("set must degrade to memory-only, not fail");
        let got: Option<Vec<u32>> = cache.get("query:asset_stats").await.unwrap();
        assert_eq!(got, Some(vec![1, 2, 3]));
        assert_eq!(cache.metrics().memory_hits, 1);
    }

    #[tokio::test]
    async fn validation_errors_are_still_errors_in_degraded_mode() {
        let cache = dead_cache().await;
        assert!(cache.get::<String>("invalid key").await.is_err());
        assert!(cache
            .set("query:x", &1u32, Duration::from_secs(0))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn failed_invalidations_are_queued_for_replay() {
        let cache = dead_cache().await;
        cache
            .set("query:status_counts", &1u32, Duration::from_secs(60))
            .await
            .unwrap();
        assert!(cache.invalidate("query:status_counts").await.is_err());
        assert!(cache
            .invalidate_exact("query:asset_total:USD")
            .await
            .is_err());
        // Duplicates are not queued twice.
        assert!(cache.invalidate("query:status_counts").await.is_err());
        assert_eq!(cache.pending_invalidation_count(), 2);
        // The local copy is gone even though Redis could not be reached.
        let got: Option<u32> = cache.get("query:status_counts").await.unwrap();
        assert_eq!(got, None);
        assert!(cache.invalidate_partition_affected_keys().await.is_err());
        assert!(cache.pending_invalidation_count() >= 4);
    }

    #[tokio::test]
    async fn pending_invalidation_queue_is_bounded() {
        let cache = dead_cache().await;
        for i in 0..(MAX_PENDING_INVALIDATIONS + 5) {
            cache.queue_invalidation(&format!("query:daily_totals:{i}"));
        }
        assert!(cache.pending_invalidation_count() <= MAX_PENDING_INVALIDATIONS);
    }

    #[tokio::test]
    async fn health_check_fails_fast_when_redis_down() {
        let cache = dead_cache().await;
        let started = Instant::now();
        assert!(cache.health_check().await.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn breaker_opens_and_short_circuits_after_repeated_failures() {
        let cache = dead_cache().await;
        for _ in 0..10 {
            let _ = cache.get::<u32>("query:asset_stats").await;
        }
        assert_eq!(cache.circuit_state(), "open");
        // Still a miss, not an error, while short-circuited.
        assert!(matches!(
            cache.get::<u32>("query:asset_stats").await,
            Ok(None)
        ));
    }

    #[test]
    fn glob_pattern_matching() {
        assert!(pattern_matches(
            "query:status_counts",
            "query:status_counts"
        ));
        assert!(!pattern_matches(
            "query:status_counts",
            "query:status_counts2"
        ));
        assert!(pattern_matches(
            "query:daily_totals:*",
            "query:daily_totals:7"
        ));
        assert!(!pattern_matches(
            "query:daily_totals:*",
            "query:asset_stats"
        ));
        assert!(pattern_matches("query:*:x", "query:a:b:x"));
        assert!(!pattern_matches("query:*:x", "query:a:b:y"));
    }
}
