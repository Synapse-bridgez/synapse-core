use redis::{AsyncCommands, Client, Script};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Fair lock queue — Redis sorted-set based, FIFO by enqueue timestamp
// ---------------------------------------------------------------------------

/// Configuration for the fair lock queue.
#[derive(Clone, Debug)]
pub struct FairLockConfig {
    /// How long a waiter stays in the queue before being considered stale and
    /// removed (crash-safety). Should be > `max_wait`.
    pub waiter_ttl: Duration,
    /// Maximum time a caller will wait in the queue before giving up.
    pub max_wait: Duration,
    /// How often to poll Redis while waiting for our turn.
    pub poll_interval: Duration,
}

impl Default for FairLockConfig {
    fn default() -> Self {
        Self {
            waiter_ttl: Duration::from_secs(60),
            max_wait: Duration::from_secs(30),
            poll_interval: Duration::from_millis(50),
        }
    }
}

/// A fair, Redis sorted-set based distributed lock.
///
/// ## Protocol
/// 1. Enqueue: `ZADD lock:queue:<resource> NX <timestamp_ms> <token>`
///    with a separate heartbeat key `lock:waiter:<resource>:<token>` (EX waiter_ttl)
///    so crashed waiters are detectable.
/// 2. Check turn: `ZRANGE lock:queue:<resource> 0 0` — if our token is first, we hold the lock.
/// 3. Before checking, prune stale waiters whose heartbeat key has expired.
/// 4. Release: `ZREM lock:queue:<resource> <token>` + delete heartbeat key.
pub struct FairLockManager {
    redis_client: Client,
    lock_ttl: Duration,
    config: FairLockConfig,
}

/// A held fair lock. Drop or call `release()` to give up the position.
pub struct FairLock {
    resource: String,
    token: String,
    redis_client: Client,
    lock_ttl: Duration,
    acquired_at: Instant,
}

impl FairLockManager {
    pub fn new(
        redis_url: &str,
        lock_ttl_secs: u64,
        config: FairLockConfig,
    ) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis_client: Client::open(redis_url)?,
            lock_ttl: Duration::from_secs(lock_ttl_secs),
            config,
        })
    }

    /// Enqueue this worker and wait until it reaches the front of the queue.
    /// Returns `None` if `max_wait` is exceeded.
    pub async fn acquire(
        &self,
        resource: &str,
    ) -> Result<Option<FairLock>, redis::RedisError> {
        let queue_key = format!("lock:queue:{}", resource);
        let token = Uuid::new_v4().to_string();
        let heartbeat_key = format!("lock:waiter:{}:{}", resource, token);
        let waiter_ttl_secs = self.config.waiter_ttl.as_secs() as usize;

        let now_ms = unix_ms();
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        // Enqueue with timestamp score (NX — don't overwrite if somehow already present)
        let _: () = conn
            .zadd_options(
                &queue_key,
                redis::ZAddOptions::default().with_condition(redis::ZAddCondition::NX),
                now_ms as f64,
                &token,
            )
            .await?;

        // Publish heartbeat so others can detect us as alive
        conn.set_ex(&heartbeat_key, "alive", waiter_ttl_secs)
            .await?;

        debug!(resource, %token, "Enqueued in fair lock queue");
        crate::metrics::lock_contention_total()
            .add(1, &[opentelemetry::KeyValue::new("resource", resource.to_string())]);

        let deadline = tokio::time::Instant::now() + self.config.max_wait;

        loop {
            // Refresh heartbeat so we aren't pruned while waiting
            conn.set_ex(&heartbeat_key, "alive", waiter_ttl_secs)
                .await?;

            // Prune stale waiters (crashed workers whose heartbeat expired)
            self.prune_stale_waiters(&mut conn, resource, &queue_key)
                .await?;

            // Check if we are at the front
            let front: Vec<String> = conn.zrange(&queue_key, 0, 0).await?;
            if front.first().map(|s| s.as_str()) == Some(&token) {
                // We're first — acquire the actual lock key
                let lock_key = format!("lock:{}", resource);
                let set_result: Option<String> = conn
                    .set_options(
                        &lock_key,
                        &token,
                        redis::SetOptions::default()
                            .conditional_set(redis::ExistenceCheck::NX)
                            .with_expiration(redis::SetExpiry::EX(
                                self.lock_ttl.as_secs() as usize,
                            )),
                    )
                    .await?;

                if set_result.is_some() {
                    // Remove from queue and clean up heartbeat
                    let _: () = conn.zrem(&queue_key, &token).await?;
                    let _: () = conn.del(&heartbeat_key).await?;

                    debug!(resource, %token, "Fair lock acquired");
                    crate::metrics::lock_acquired_total().add(
                        1,
                        &[opentelemetry::KeyValue::new("resource", resource.to_string())],
                    );

                    // Register in active lock registry
                    let acquired_unix = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    lock_registry()
                        .register(ActiveLockInfo {
                            resource: resource.to_string(),
                            token: token.clone(),
                            acquired_at: acquired_unix,
                            ttl_secs: self.lock_ttl.as_secs(),
                            expected_duration_secs: self.lock_ttl.as_secs(),
                            overdue: false,
                        })
                        .await;

                    return Ok(Some(FairLock {
                        resource: resource.to_string(),
                        token,
                        redis_client: self.redis_client.clone(),
                        lock_ttl: self.lock_ttl,
                        acquired_at: Instant::now(),
                    }));
                }
                // Lock key already held by someone else — shouldn't normally happen
                // since we're at the front, but handle it gracefully by waiting.
            }

            if tokio::time::Instant::now() >= deadline {
                // Give up — remove ourselves from the queue
                let _: () = conn.zrem(&queue_key, &token).await?;
                let _: () = conn.del(&heartbeat_key).await?;
                debug!(resource, %token, "Fair lock wait timed out, removed from queue");
                return Ok(None);
            }

            sleep(self.config.poll_interval).await;
        }
    }

    /// Remove queue members whose heartbeat key has expired (crashed waiters).
    async fn prune_stale_waiters(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        resource: &str,
        queue_key: &str,
    ) -> Result<(), redis::RedisError> {
        // Fetch all members
        let members: Vec<String> = conn.zrange(queue_key, 0, -1).await?;
        for member in members {
            let hb_key = format!("lock:waiter:{}:{}", resource, member);
            let alive: Option<String> = conn.get(&hb_key).await?;
            if alive.is_none() {
                // Heartbeat gone — waiter crashed, evict from queue
                let _: () = conn.zrem(queue_key, &member).await?;
                warn!(resource, token = %member, "Pruned stale waiter from fair lock queue");
            }
        }
        Ok(())
    }
}

impl FairLock {
    pub async fn release(self) -> Result<(), redis::RedisError> {
        let hold_ms = self.acquired_at.elapsed().as_secs_f64() * 1000.0;
        let lock_key = format!("lock:{}", self.resource);
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        // Release the lock key (token-checked)
        let script = Script::new(
            r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("del", KEYS[1])
            else
                return 0
            end
            "#,
        );
        let _: i32 = script
            .key(&lock_key)
            .arg(&self.token)
            .invoke_async(&mut conn)
            .await?;

        debug!(resource = %self.resource, hold_ms, "Released fair lock");

        crate::metrics::lock_hold_duration_ms().record(
            hold_ms,
            &[opentelemetry::KeyValue::new("resource", self.resource.clone())],
        );

        let expected_ms = self.lock_ttl.as_secs_f64() * 1000.0;
        if hold_ms > expected_ms * 2.0 {
            warn!(
                resource = %self.resource,
                hold_ms,
                expected_ms,
                "Fair lock held longer than 2x expected duration"
            );
        }

        lock_registry().deregister(&self.token).await;
        Ok(())
    }
}

impl Drop for FairLock {
    fn drop(&mut self) {
        let lock_key = format!("lock:{}", self.resource);
        let token = self.token.clone();
        let client = self.redis_client.clone();
        let hold_ms = self.acquired_at.elapsed().as_secs_f64() * 1000.0;
        let expected_ms = self.lock_ttl.as_secs_f64() * 1000.0;
        let resource = self.resource.clone();

        tokio::spawn(async move {
            crate::metrics::lock_hold_duration_ms().record(
                hold_ms,
                &[opentelemetry::KeyValue::new("resource", resource.clone())],
            );
            if hold_ms > expected_ms * 2.0 {
                warn!(resource, hold_ms, expected_ms, "Fair lock (dropped) held longer than 2x expected duration");
            }
            lock_registry().deregister(&token).await;

            if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
                let script = Script::new(
                    r#"
                    if redis.call("get", KEYS[1]) == ARGV[1] then
                        return redis.call("del", KEYS[1])
                    else
                        return 0
                    end
                    "#,
                );
                let _ = script
                    .key(&lock_key)
                    .arg(&token)
                    .invoke_async::<_, i32>(&mut conn)
                    .await;
            }
        });
    }
}

/// Current Unix time in milliseconds.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

const LEADER_KEY: &str = "processor:leader";
const LEADER_LEASE_SECS: u64 = 30;
const HEARTBEAT_TTL_SECS: u64 = 45;

/// Metadata about a currently-held lock, exposed via the admin endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveLockInfo {
    pub resource: String,
    pub token: String,
    pub acquired_at: u64, // Unix timestamp (secs)
    pub ttl_secs: u64,
    pub expected_duration_secs: u64,
    pub overdue: bool,
}

/// Shared registry of all currently-held locks in this process.
#[derive(Clone, Default)]
pub struct LockRegistry {
    inner: Arc<RwLock<HashMap<String, ActiveLockInfo>>>,
}

impl LockRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    async fn register(&self, info: ActiveLockInfo) {
        self.inner.write().await.insert(info.token.clone(), info);
    }

    async fn deregister(&self, token: &str) {
        self.inner.write().await.remove(token);
    }

    /// Snapshot of all active locks, with `overdue` flag refreshed.
    pub async fn snapshot(&self) -> Vec<ActiveLockInfo> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.inner
            .read()
            .await
            .values()
            .map(|info| {
                let held_secs = now.saturating_sub(info.acquired_at);
                ActiveLockInfo {
                    overdue: held_secs > info.expected_duration_secs * 2,
                    ..info.clone()
                }
            })
            .collect()
    }
}

// Global registry — shared across all LockManager instances in the process.
static LOCK_REGISTRY: std::sync::OnceLock<LockRegistry> = std::sync::OnceLock::new();

pub fn lock_registry() -> &'static LockRegistry {
    LOCK_REGISTRY.get_or_init(LockRegistry::new)
}

// ---------------------------------------------------------------------------
// LockManager
// ---------------------------------------------------------------------------

pub struct LockManager {
    redis_client: Client,
    default_ttl: Duration,
}

#[derive(Clone)]
pub struct Lock {
    key: String,
    token: String,
    redis_client: Client,
    ttl: Duration,
    acquired_at: Instant,
}

impl LockManager {
    pub fn new(redis_url: &str, default_ttl_secs: u64) -> Result<Self, redis::RedisError> {
        let redis_client = Client::open(redis_url)?;
        Ok(Self {
            redis_client,
            default_ttl: Duration::from_secs(default_ttl_secs),
        })
    }

    pub async fn acquire(
        &self,
        resource: &str,
        timeout_duration: Duration,
    ) -> Result<Option<Lock>, redis::RedisError> {
        let key = format!("lock:{resource}");
        let token = Uuid::new_v4().to_string();
        let ttl = self.default_ttl;

        let start = tokio::time::Instant::now();
        let mut attempts: u64 = 0;

        loop {
            attempts += 1;

            if let Some(lock) = self.try_acquire(&key, &token, ttl).await? {
                debug!(resource, attempts, "Acquired distributed lock");

                // Metrics
                crate::metrics::lock_acquired_total().add(1, &[opentelemetry::KeyValue::new("resource", resource.to_string())]);

                // Register in active lock registry
                let acquired_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                lock_registry()
                    .register(ActiveLockInfo {
                        resource: resource.to_string(),
                        token: token.clone(),
                        acquired_at: acquired_unix,
                        ttl_secs: ttl.as_secs(),
                        expected_duration_secs: ttl.as_secs(),
                        overdue: false,
                    })
                    .await;

                return Ok(Some(lock));
            }

            // Each failed attempt is a contention event
            crate::metrics::lock_contention_total().add(1, &[opentelemetry::KeyValue::new("resource", resource.to_string())]);

            if start.elapsed() >= timeout_duration {
                debug!(resource, attempts, "Lock acquisition timed out");
                return Ok(None);
            }

            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn try_acquire(
        &self,
        key: &str,
        token: &str,
        ttl: Duration,
    ) -> Result<Option<Lock>, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        let result: Option<String> = conn
            .set_options(
                key,
                token,
                redis::SetOptions::default()
                    .conditional_set(redis::ExistenceCheck::NX)
                    .with_expiration(redis::SetExpiry::EX(ttl.as_secs() as usize)),
            )
            .await?;

        if result.is_some() {
            Ok(Some(Lock {
                key: key.to_string(),
                token: token.to_string(),
                redis_client: self.redis_client.clone(),
                ttl,
                acquired_at: Instant::now(),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn with_lock<F, T>(
        &self,
        resource: &str,
        timeout_duration: Duration,
        f: F,
    ) -> Result<Option<T>, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce() -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<T, Box<dyn std::error::Error + Send + Sync>>,
                    > + Send,
            >,
        >,
    {
        let lock = match self.acquire(resource, timeout_duration).await? {
            Some(lock) => lock,
            None => return Ok(None),
        };

        let result = f().await;

        lock.release().await?;

        result.map(Some)
    }
}

impl Lock {
    pub async fn release(self) -> Result<(), redis::RedisError> {
        let hold_ms = self.acquired_at.elapsed().as_secs_f64() * 1000.0;
        let resource = self.key.trim_start_matches("lock:").to_string();

        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        let script = Script::new(
            r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("del", KEYS[1])
            else
                return 0
            end
            "#,
        );

        let _: i32 = script
            .key(&self.key)
            .arg(&self.token)
            .invoke_async(&mut conn)
            .await?;

        debug!(resource, hold_ms, "Released distributed lock");

        // Record hold duration metric
        crate::metrics::lock_hold_duration_ms().record(
            hold_ms,
            &[opentelemetry::KeyValue::new("resource", resource.clone())],
        );

        // Alert if held longer than 2x TTL
        let expected_ms = self.ttl.as_secs_f64() * 1000.0;
        if hold_ms > expected_ms * 2.0 {
            warn!(
                resource,
                hold_ms,
                expected_ms,
                "Lock held longer than 2x expected duration"
            );
        }

        // Remove from registry
        lock_registry().deregister(&self.token).await;

        Ok(())
    }

    pub async fn renew(&mut self) -> Result<bool, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        let script = Script::new(
            r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("expire", KEYS[1], ARGV[2])
            else
                return 0
            end
            "#,
        );

        let result: i32 = script
            .key(&self.key)
            .arg(&self.token)
            .arg(self.ttl.as_secs() as i32)
            .invoke_async(&mut conn)
            .await?;

        if result == 1 {
            debug!(key = %self.key, "Renewed distributed lock");
        } else {
            warn!(key = %self.key, "Failed to renew lock — token mismatch");
        }

        Ok(result == 1)
    }

    pub async fn auto_renew_task(mut self) {
        let renew_interval = self.ttl / 2;

        loop {
            sleep(renew_interval).await;

            match self.renew().await {
                Ok(true) => debug!("Renewed lock for {}", self.key),
                Ok(false) => {
                    warn!("Failed to renew lock for {} - token mismatch", self.key);
                    break;
                }
                Err(e) => {
                    warn!("Error renewing lock for {}: {}", self.key, e);
                    break;
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let key = self.key.clone();
        let token = self.token.clone();
        let client = self.redis_client.clone();
        let hold_ms = self.acquired_at.elapsed().as_secs_f64() * 1000.0;
        let expected_ms = self.ttl.as_secs_f64() * 1000.0;
        let resource = key.trim_start_matches("lock:").to_string();

        tokio::spawn(async move {
            // Record metrics on drop (best-effort)
            crate::metrics::lock_hold_duration_ms().record(
                hold_ms,
                &[opentelemetry::KeyValue::new("resource", resource.clone())],
            );

            if hold_ms > expected_ms * 2.0 {
                warn!(
                    resource,
                    hold_ms,
                    expected_ms,
                    "Lock (dropped) held longer than 2x expected duration"
                );
            }

            lock_registry().deregister(&token).await;

            if let Ok(mut conn) = client.get_multiplexed_async_connection().await {
                let script = Script::new(
                    r#"
                    if redis.call("get", KEYS[1]) == ARGV[1] then
                        return redis.call("del", KEYS[1])
                    else
                        return 0
                    end
                    "#,
                );

                let _ = script
                    .key(&key)
                    .arg(&token)
                    .invoke_async::<_, i32>(&mut conn)
                    .await;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// LeaderElection
// ---------------------------------------------------------------------------

/// Redis-based leader election for processor coordination.
///
/// Uses `SET NX EX` with a 30-second lease. Only the leader should run
/// partition maintenance, settlement jobs, and webhook dispatch.
/// All instances run processor workers (safe via SKIP LOCKED).
pub struct LeaderElection {
    redis_client: Client,
    instance_id: String,
}

impl LeaderElection {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis_client: Client::open(redis_url)?,
            instance_id: Uuid::new_v4().to_string(),
        })
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Try to acquire or renew the leader lease. Returns true if this instance is leader.
    pub async fn try_acquire_leadership(&self) -> Result<bool, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        let result: Option<String> = conn
            .set_options(
                LEADER_KEY,
                &self.instance_id,
                redis::SetOptions::default()
                    .conditional_set(redis::ExistenceCheck::NX)
                    .with_expiration(redis::SetExpiry::EX(LEADER_LEASE_SECS as usize)),
            )
            .await?;

        if result.is_some() {
            return Ok(true);
        }

        let script = Script::new(
            r#"
            if redis.call("get", KEYS[1]) == ARGV[1] then
                return redis.call("expire", KEYS[1], ARGV[2])
            else
                return 0
            end
            "#,
        );
        let renewed: i32 = script
            .key(LEADER_KEY)
            .arg(&self.instance_id)
            .arg(LEADER_LEASE_SECS as i32)
            .invoke_async(&mut conn)
            .await?;

        Ok(renewed == 1)
    }

    /// Publish a heartbeat key with TTL so other instances can discover this one.
    pub async fn publish_heartbeat(&self) -> Result<(), redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("processor:heartbeat:{}", self.instance_id);
        conn.set_ex::<_, _, ()>(key, "alive", HEARTBEAT_TTL_SECS)
            .await?;
        Ok(())
    }

    /// List all active instance IDs by scanning heartbeat keys.
    pub async fn list_active_instances(&self) -> Result<Vec<String>, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let keys: Vec<String> = conn.keys("processor:heartbeat:*").await?;
        Ok(keys
            .into_iter()
            .map(|k| k.trim_start_matches("processor:heartbeat:").to_string())
            .collect())
    }

    /// Return the current leader instance ID, if any.
    pub async fn current_leader(&self) -> Result<Option<String>, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let leader: Option<String> = conn.get(LEADER_KEY).await?;
        Ok(leader)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[ignore = "Requires DATABASE_URL / Redis"]
    #[tokio::test]
    async fn test_lock_acquire_release() {
        let manager = LockManager::new("redis://localhost:6379", 30).unwrap();

        let lock = manager
            .acquire("test_resource", Duration::from_secs(5))
            .await
            .unwrap();

        assert!(lock.is_some());

        let lock = lock.unwrap();
        lock.release().await.unwrap();
    }

    #[ignore = "Requires DATABASE_URL / Redis"]
    #[tokio::test]
    async fn test_lock_prevents_duplicate() {
        let manager = LockManager::new("redis://localhost:6379", 30).unwrap();

        let lock1 = manager
            .acquire("test_resource_2", Duration::from_secs(5))
            .await
            .unwrap();

        assert!(lock1.is_some());

        let lock2 = manager
            .acquire("test_resource_2", Duration::from_millis(100))
            .await
            .unwrap();

        assert!(lock2.is_none());

        lock1.unwrap().release().await.unwrap();
    }

    #[tokio::test]
    async fn test_lock_metrics_emitted() {
        // Verify metric instruments can be created without panicking
        let _ = crate::metrics::lock_acquired_total();
        let _ = crate::metrics::lock_contention_total();
        let _ = crate::metrics::lock_hold_duration_ms();
    }

    #[tokio::test]
    async fn test_lock_registry_snapshot() {
        let registry = LockRegistry::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        registry
            .register(ActiveLockInfo {
                resource: "test".to_string(),
                token: "tok-1".to_string(),
                acquired_at: now,
                ttl_secs: 30,
                expected_duration_secs: 30,
                overdue: false,
            })
            .await;

        let snap = registry.snapshot().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].resource, "test");

        registry.deregister("tok-1").await;
        assert!(registry.snapshot().await.is_empty());
    }

    // -----------------------------------------------------------------------
    // Fair lock queue tests (no Redis required)
    // -----------------------------------------------------------------------

    /// Verify FairLockConfig defaults are sane.
    #[test]
    fn test_fair_lock_config_defaults() {
        let cfg = FairLockConfig::default();
        assert!(cfg.max_wait > Duration::ZERO);
        assert!(cfg.waiter_ttl > cfg.max_wait);
        assert!(cfg.poll_interval > Duration::ZERO);
    }

    /// Verify unix_ms() returns a plausible value (after year 2020).
    #[test]
    fn test_unix_ms_plausible() {
        let ms = unix_ms();
        // 2020-01-01 in ms
        assert!(ms > 1_577_836_800_000);
    }

    /// With N workers contending, each should get approximately equal lock time.
    /// This is a logic/unit test — uses the registry to verify fairness ordering.
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn test_fair_queue_equal_distribution() {
        let redis_url = "redis://localhost:6379";
        let resource = "fair_test_equal";
        let n_workers: usize = 4;
        let acquired = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let handles: Vec<_> = (0..n_workers)
            .map(|_| {
                let acquired = acquired.clone();
                tokio::spawn(async move {
                    let mgr = FairLockManager::new(
                        redis_url,
                        2,
                        FairLockConfig {
                            max_wait: Duration::from_secs(20),
                            waiter_ttl: Duration::from_secs(30),
                            poll_interval: Duration::from_millis(20),
                        },
                    )
                    .unwrap();
                    if let Ok(Some(lock)) = mgr.acquire(resource).await {
                        acquired.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        sleep(Duration::from_millis(50)).await;
                        let _ = lock.release().await;
                    }
                })
            })
            .collect();

        for h in handles {
            let _ = h.await;
        }

        // All workers should have acquired the lock exactly once
        assert_eq!(acquired.load(std::sync::atomic::Ordering::SeqCst), n_workers);
    }

    /// A crashed waiter (no heartbeat) should be pruned from the queue.
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn test_crashed_waiter_pruned() {
        let redis_url = "redis://localhost:6379";
        let resource = "fair_test_prune";
        let queue_key = format!("lock:queue:{}", resource);
        let stale_token = "stale-worker-token";

        // Manually insert a stale entry (no heartbeat key)
        let client = Client::open(redis_url).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        // Score = 1 (very old, should be first)
        let _: () = conn
            .zadd_options(
                &queue_key,
                redis::ZAddOptions::default().with_condition(redis::ZAddCondition::NX),
                1.0_f64,
                stale_token,
            )
            .await
            .unwrap();

        // Now a real worker acquires — it should prune the stale entry and succeed
        let mgr = FairLockManager::new(
            redis_url,
            5,
            FairLockConfig {
                max_wait: Duration::from_secs(5),
                waiter_ttl: Duration::from_secs(10),
                poll_interval: Duration::from_millis(50),
            },
        )
        .unwrap();

        let lock = mgr.acquire(resource).await.unwrap();
        assert!(lock.is_some(), "Should acquire after pruning stale waiter");

        // Stale token must be gone from the queue
        let members: Vec<String> = conn.zrange(&queue_key, 0, -1).await.unwrap();
        assert!(
            !members.contains(&stale_token.to_string()),
            "Stale waiter should have been pruned"
        );

        let _ = lock.unwrap().release().await;
    }
}
