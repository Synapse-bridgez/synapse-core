use redis::{AsyncCommands, Client, Script};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};
use uuid::Uuid;

const LEADER_KEY: &str = "processor:leader";
const LEADER_LEASE_SECS: u64 = 30;
const HEARTBEAT_TTL_SECS: u64 = 45;

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
        let key = format!("lock:{}", resource);
        let token = Uuid::new_v4().to_string();
        let ttl = self.default_ttl;

        let start = tokio::time::Instant::now();

        loop {
            if let Some(lock) = self.try_acquire(&key, &token, ttl).await? {
                debug!("Acquired lock for {}", resource);
                return Ok(Some(lock));
            }

            if start.elapsed() >= timeout_duration {
                debug!("Lock acquisition timeout for {}", resource);
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

        // SET key token NX EX ttl_seconds
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
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        // Lua script to ensure we only delete if token matches
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

        debug!("Released lock for {}", self.key);
        Ok(())
    }

    pub async fn renew(&mut self) -> Result<bool, redis::RedisError> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;

        // Lua script to renew only if token matches
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
        // Best effort release on drop
        let key = self.key.clone();
        let token = self.token.clone();
        let client = self.redis_client.clone();

        tokio::spawn(async move {
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

        // Try SET NX EX first
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

        // If we already hold the lease, renew it
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
        conn.set_ex(key, "alive", HEARTBEAT_TTL_SECS as usize)
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

        // Try to acquire same lock
        let lock2 = manager
            .acquire("test_resource_2", Duration::from_millis(100))
            .await
            .unwrap();

        assert!(lock2.is_none());

        lock1.unwrap().release().await.unwrap();
    }
}
