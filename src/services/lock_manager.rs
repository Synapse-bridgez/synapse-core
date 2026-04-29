use redis::{AsyncCommands, Client, Script};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, warn};
use uuid::Uuid;

const LEADER_KEY: &str = "processor:leader";
const LEADER_LEASE_SECS: u64 = 30;
const HEARTBEAT_TTL_SECS: u64 = 45;

// ---------------------------------------------------------------------------
// Active lock registry
// ---------------------------------------------------------------------------

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
}
