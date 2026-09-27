use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::RwLock;
use vaultrs::auth::approle;
use vaultrs::client::{Client, VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

/// Grace period during which the previous secret remains valid after rotation.
const ROTATION_GRACE_PERIOD: Duration = Duration::from_secs(300);
/// How often to poll Vault for updated secrets. This is now a *fallback*
/// cadence, not the primary detection path: `start_refresh_task` also
/// subscribes to `ROTATION_CHANNEL` so a rotation detected by any one
/// instance's poll is pushed to every other instance immediately instead of
/// each instance waiting up to a full `REFRESH_INTERVAL` on its own clock.
/// See the coordination-gap issue this fixes for why the two independent
/// polling clocks alone could double the effective grace period fleet-wide.
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);
/// Redis pub/sub channel used to fan out "a rotation was just detected" to
/// every instance. Reuses the same Redis infrastructure/pattern already
/// proven for cross-instance coordination in `circuit_breaker.rs`.
const ROTATION_CHANNEL: &str = "secrets:rotation";

/// A double-buffered secret: keeps current and previous value.
/// During the grace period both are accepted for signature validation.
#[derive(Clone, Debug)]
pub struct RotatingSecret {
    pub current: String,
    pub previous: Option<(String, Instant)>,
}

impl RotatingSecret {
    pub fn new(value: String) -> Self {
        Self {
            current: value,
            previous: None,
        }
    }

    /// Returns all currently-valid values: current first, then previous if still in grace period.
    pub fn valid_values(&self) -> Vec<&str> {
        let mut values = vec![self.current.as_str()];
        if let Some((prev, rotated_at)) = &self.previous {
            if rotated_at.elapsed() < ROTATION_GRACE_PERIOD {
                values.push(prev.as_str());
            }
        }
        values
    }

    /// Rotate to a new value, demoting current to previous.
    pub fn rotate(&mut self, new_value: String) {
        let old = std::mem::replace(&mut self.current, new_value);
        self.previous = Some((old, Instant::now()));
    }

    /// Checks `candidate` against this secret's valid values, distinguishing
    /// which one matched. `Some(true)` = matched `current`; `Some(false)` =
    /// matched `previous` within the grace period; `None` = matched neither.
    /// Callers use the `Some(false)` case to record the
    /// `secrets_previous_value_verified_total` metric — a caller still
    /// presenting the old secret well after a rotation is exactly the signal
    /// that motivates bounding the fleet-wide propagation window.
    pub fn verify(&self, candidate: &str) -> Option<bool> {
        if candidate == self.current {
            return Some(true);
        }
        if let Some((prev, rotated_at)) = &self.previous {
            if rotated_at.elapsed() < ROTATION_GRACE_PERIOD && candidate == prev {
                return Some(false);
            }
        }
        None
    }
}

/// Thread-safe store of rotating secrets shared across the application.
#[derive(Clone)]
pub struct SecretsStore {
    pub anchor_webhook_secret: Arc<RwLock<RotatingSecret>>,
    pub admin_api_key: Arc<RwLock<RotatingSecret>>,
}

impl SecretsStore {
    pub fn new(anchor_webhook_secret: String, admin_api_key: String) -> Self {
        Self {
            anchor_webhook_secret: Arc::new(RwLock::new(RotatingSecret::new(
                anchor_webhook_secret,
            ))),
            admin_api_key: Arc::new(RwLock::new(RotatingSecret::new(admin_api_key))),
        }
    }

    /// Returns all valid anchor webhook secret values (current + grace-period previous).
    pub async fn valid_webhook_secrets(&self) -> Vec<String> {
        self.anchor_webhook_secret
            .read()
            .await
            .valid_values()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Returns all valid admin API key values (current + grace-period previous).
    pub async fn valid_admin_keys(&self) -> Vec<String> {
        self.admin_api_key
            .read()
            .await
            .valid_values()
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Verifies `candidate` against the admin API key, recording
    /// `secrets_previous_value_verified_total` when it only matches the
    /// grace-period previous value rather than current.
    pub async fn verify_admin_key(&self, candidate: &str) -> bool {
        Self::verify_and_record(&self.admin_api_key, candidate, "admin_api_key").await
    }

    /// Verifies `candidate` against the anchor webhook secret, recording
    /// `secrets_previous_value_verified_total` when it only matches the
    /// grace-period previous value rather than current.
    pub async fn verify_webhook_secret(&self, candidate: &str) -> bool {
        Self::verify_and_record(
            &self.anchor_webhook_secret,
            candidate,
            "anchor_webhook_secret",
        )
        .await
    }

    async fn verify_and_record(
        secret: &Arc<RwLock<RotatingSecret>>,
        candidate: &str,
        secret_name: &'static str,
    ) -> bool {
        match secret.read().await.verify(candidate) {
            Some(true) => true,
            Some(false) => {
                crate::metrics::secrets_previous_value_verified_total()
                    .add(1, &[opentelemetry::KeyValue::new("secret", secret_name)]);
                true
            }
            None => false,
        }
    }
}

pub struct SecretsManager {
    client: VaultClient,
    kv_mount: String,
}

impl SecretsManager {
    pub async fn new() -> Result<Self> {
        let vault_addr =
            env::var("VAULT_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".to_string());
        let role_id = env::var("VAULT_ROLE_ID").context("VAULT_ROLE_ID is required")?;
        let secret_id = env::var("VAULT_SECRET_ID").context("VAULT_SECRET_ID is required")?;
        let auth_mount =
            env::var("VAULT_AUTH_MOUNT").unwrap_or_else(|_| "auth/approle".to_string());
        let kv_mount = env::var("VAULT_KV_MOUNT").unwrap_or_else(|_| "secret".to_string());

        let mut client = VaultClient::new(
            VaultClientSettingsBuilder::default()
                .address(&vault_addr)
                .build()
                .context("failed to build Vault client settings")?,
        )
        .context("failed to create Vault client")?;

        let auth = approle::login(&client, &auth_mount, &role_id, &secret_id)
            .await
            .context("failed to authenticate to Vault with AppRole")?;
        client.set_token(&auth.client_token);

        Ok(Self { client, kv_mount })
    }

    pub async fn get_db_password(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "database")
            .await
            .context("failed to read secret/database from Vault")?;

        secret
            .get("password")
            .cloned()
            .context("password key not found in Vault secret/database")
    }

    pub async fn get_anchor_secret(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "anchor")
            .await
            .context("failed to read secret/anchor from Vault")?;

        secret
            .get("secret")
            .cloned()
            .context("secret key not found in Vault secret/anchor")
    }

    pub async fn get_admin_api_key(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "admin")
            .await
            .context("failed to read secret/admin from Vault")?;

        secret
            .get("api_key")
            .cloned()
            .context("api_key not found in Vault secret/admin")
    }

    /// Spawn the background tasks that keep secrets fresh and coordinated
    /// across the fleet:
    ///
    /// - A poll loop (unchanged cadence, `REFRESH_INTERVAL`) that remains the
    ///   sole source of truth for *whether* a rotation happened, and the
    ///   fallback detection path if pub/sub is unavailable.
    /// - A dedicated pub/sub listener (reusing the Redis infrastructure
    ///   already proven for `circuit_breaker.rs`'s cross-instance
    ///   coordination) that wakes the poll loop immediately when *any*
    ///   instance's poll detects and announces a rotation, instead of this
    ///   instance waiting up to its own `REFRESH_INTERVAL`.
    ///
    /// Without this, two instances' independently-clocked polls could leave
    /// the old secret valid fleet-wide for up to `REFRESH_INTERVAL +
    /// ROTATION_GRACE_PERIOD` — double what either constant suggests alone.
    pub fn start_refresh_task(self, store: SecretsStore, redis_url: String) {
        let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<i64>();

        // Dedicated listener: owns its own pub/sub connection, reconnects
        // with backoff if it drops, and forwards the publish timestamp of
        // any rotation notification. Kept separate from the refresh loop
        // below so a Redis outage degrades this to poll-only (the refresh
        // loop's `tokio::select!` simply never receives from `notify_rx`)
        // rather than blocking secret refresh entirely.
        {
            let redis_url = redis_url.clone();
            tokio::spawn(async move {
                loop {
                    match Self::connect_rotation_pubsub(&redis_url).await {
                        Some(mut pubsub) => {
                            let mut stream = pubsub.on_message();
                            while let Some(msg) = stream.next().await {
                                let published_at_ms = msg
                                    .get_payload::<String>()
                                    .ok()
                                    .and_then(|p| p.parse::<i64>().ok())
                                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
                                let _ = notify_tx.send(published_at_ms);
                            }
                            // Stream ended: connection dropped; fall through to reconnect.
                        }
                        None => {
                            crate::metrics::secrets_rotation_pubsub_unavailable_total().add(1, &[]);
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });
        }

        let publish_client = redis::Client::open(redis_url.as_str()).ok();
        if publish_client.is_none() {
            tracing::warn!(
                "secrets_rotation: could not build Redis client for rotation \
                 announcements ({redis_url}); this instance will still detect \
                 its own rotations via polling but cannot notify others"
            );
        }

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REFRESH_INTERVAL);
            interval.tick().await; // skip the immediate first tick
            loop {
                let published_at_ms = tokio::select! {
                    _ = interval.tick() => None,
                    Some(ts) = notify_rx.recv() => Some(ts),
                };
                let via_pubsub = published_at_ms.is_some();
                let lag_ms = published_at_ms
                    .map(|ts| (chrono::Utc::now().timestamp_millis() - ts).max(0) as f64)
                    .unwrap_or(0.0);

                tracing::info!(
                    via_pubsub,
                    "secrets_rotation: refreshing secrets from Vault"
                );

                let mut rotated_any = false;

                match self.get_anchor_secret().await {
                    Ok(new_secret) => {
                        let mut lock = store.anchor_webhook_secret.write().await;
                        if lock.current != new_secret {
                            lock.rotate(new_secret);
                            rotated_any = true;
                            crate::metrics::secrets_rotation_detection_lag_ms().record(
                                lag_ms,
                                &[
                                    opentelemetry::KeyValue::new("secret", "anchor_webhook_secret"),
                                    opentelemetry::KeyValue::new("via_pubsub", via_pubsub),
                                ],
                            );
                            tracing::info!(
                                "secrets_rotation: anchor_webhook_secret rotated; \
                                 previous value valid for {}s grace period",
                                ROTATION_GRACE_PERIOD.as_secs()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("secrets_rotation: failed to refresh anchor secret: {e}");
                    }
                }

                match self.get_admin_api_key().await {
                    Ok(new_key) => {
                        let mut lock = store.admin_api_key.write().await;
                        if lock.current != new_key {
                            lock.rotate(new_key);
                            rotated_any = true;
                            crate::metrics::secrets_rotation_detection_lag_ms().record(
                                lag_ms,
                                &[
                                    opentelemetry::KeyValue::new("secret", "admin_api_key"),
                                    opentelemetry::KeyValue::new("via_pubsub", via_pubsub),
                                ],
                            );
                            tracing::info!(
                                "secrets_rotation: admin_api_key rotated; \
                                 previous value valid for {}s grace period",
                                ROTATION_GRACE_PERIOD.as_secs()
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("secrets_rotation: failed to refresh admin key: {e}");
                    }
                }

                // Announce to the rest of the fleet so their grace-period
                // clocks start now instead of on their own next poll tick.
                // Redundant if another instance already announced the same
                // rotation (harmless: recipients only rotate on an actual
                // value change).
                if rotated_any {
                    if let Some(client) = &publish_client {
                        if let Ok(mut conn) = client.get_async_connection().await {
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            let _: std::result::Result<(), _> = redis::cmd("PUBLISH")
                                .arg(ROTATION_CHANNEL)
                                .arg(now_ms.to_string())
                                .query_async(&mut conn)
                                .await;
                        }
                    }
                }
            }
        });
    }

    /// Best-effort: connects and subscribes to `ROTATION_CHANNEL`, returning
    /// `None` (rather than erroring) on any failure so callers can fall back
    /// to poll-only detection.
    async fn connect_rotation_pubsub(redis_url: &str) -> Option<redis::aio::PubSub> {
        let client = match redis::Client::open(redis_url) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("secrets_rotation: failed to build Redis client: {e}");
                return None;
            }
        };
        let conn = match client.get_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "secrets_rotation: pub/sub connection unavailable, falling back \
                     to poll-only detection: {e}"
                );
                return None;
            }
        };
        let mut pubsub = conn.into_pubsub();
        if let Err(e) = pubsub.subscribe(ROTATION_CHANNEL).await {
            tracing::warn!("secrets_rotation: failed to subscribe to {ROTATION_CHANNEL}: {e}");
            return None;
        }
        tracing::info!(
            "secrets_rotation: subscribed to {ROTATION_CHANNEL} for fleet-wide \
             rotation notifications"
        );
        Some(pubsub)
    }
}

/// Simple secret retrieval from environment variables with caching
pub mod env_secrets {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    #[derive(Clone)]
    pub struct EnvSecretsManager {
        cache: Arc<RwLock<HashMap<String, String>>>,
    }

    impl EnvSecretsManager {
        pub fn new() -> Self {
            Self {
                cache: Arc::new(RwLock::new(HashMap::new())),
            }
        }

        pub fn get_secret(&self, key: &str) -> Result<String, String> {
            // Check cache first
            {
                let cache = self.cache.read().unwrap();
                if let Some(value) = cache.get(key) {
                    return Ok(value.clone());
                }
            }

            // Retrieve from environment
            let value = std::env::var(key).map_err(|_| format!("Secret '{key}' not found"))?;

            // Cache the value
            {
                let mut cache = self.cache.write().unwrap();
                cache.insert(key.to_string(), value.clone());
            }

            Ok(value)
        }

        pub fn rotate_secret(&self, key: &str, new_value: String) {
            let mut cache = self.cache.write().unwrap();
            cache.insert(key.to_string(), new_value);
        }

        pub fn clear_cache(&self) {
            let mut cache = self.cache.write().unwrap();
            cache.clear();
        }

        pub fn cache_size(&self) -> usize {
            let cache = self.cache.read().unwrap();
            cache.len()
        }
    }

    impl Default for EnvSecretsManager {
        fn default() -> Self {
            Self::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::env_secrets::EnvSecretsManager;
    use std::env;

    #[test]
    fn test_secret_retrieval_from_env() {
        // Set up test environment variable
        env::set_var("TEST_SECRET_KEY", "test_secret_value");

        let manager = EnvSecretsManager::new();
        let result = manager.get_secret("TEST_SECRET_KEY");

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "test_secret_value");

        // Clean up
        env::remove_var("TEST_SECRET_KEY");
    }

    #[test]
    fn test_secret_caching() {
        // Set up test environment variable
        env::set_var("CACHED_SECRET", "cached_value");

        let manager = EnvSecretsManager::new();

        // First retrieval - should cache
        let result1 = manager.get_secret("CACHED_SECRET");
        assert!(result1.is_ok());
        assert_eq!(manager.cache_size(), 1);

        // Remove from environment
        env::remove_var("CACHED_SECRET");

        // Second retrieval - should use cache
        let result2 = manager.get_secret("CACHED_SECRET");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "cached_value");
    }

    #[test]
    fn test_secret_missing_error() {
        let manager = EnvSecretsManager::new();

        // Try to get non-existent secret
        let result = manager.get_secret("NON_EXISTENT_SECRET");

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Secret 'NON_EXISTENT_SECRET' not found"));
    }

    #[test]
    fn test_secret_rotation() {
        // Set up initial secret
        env::set_var("ROTATABLE_SECRET", "old_value");

        let manager = EnvSecretsManager::new();

        // Get initial value
        let result1 = manager.get_secret("ROTATABLE_SECRET");
        assert_eq!(result1.unwrap(), "old_value");

        // Rotate secret
        manager.rotate_secret("ROTATABLE_SECRET", "new_value".to_string());

        // Get rotated value
        let result2 = manager.get_secret("ROTATABLE_SECRET");
        assert_eq!(result2.unwrap(), "new_value");

        // Clean up
        env::remove_var("ROTATABLE_SECRET");
    }

    #[test]
    fn test_cache_clear() {
        env::set_var("CLEAR_TEST_1", "value1");
        env::set_var("CLEAR_TEST_2", "value2");

        let manager = EnvSecretsManager::new();

        // Cache multiple secrets
        manager.get_secret("CLEAR_TEST_1").unwrap();
        manager.get_secret("CLEAR_TEST_2").unwrap();
        assert_eq!(manager.cache_size(), 2);

        // Clear cache
        manager.clear_cache();
        assert_eq!(manager.cache_size(), 0);

        // Clean up
        env::remove_var("CLEAR_TEST_1");
        env::remove_var("CLEAR_TEST_2");
    }

    #[test]
    fn test_multiple_secret_retrievals() {
        env::set_var("SECRET_1", "value1");
        env::set_var("SECRET_2", "value2");
        env::set_var("SECRET_3", "value3");

        let manager = EnvSecretsManager::new();

        let result1 = manager.get_secret("SECRET_1");
        let result2 = manager.get_secret("SECRET_2");
        let result3 = manager.get_secret("SECRET_3");

        assert_eq!(result1.unwrap(), "value1");
        assert_eq!(result2.unwrap(), "value2");
        assert_eq!(result3.unwrap(), "value3");
        assert_eq!(manager.cache_size(), 3);

        // Clean up
        env::remove_var("SECRET_1");
        env::remove_var("SECRET_2");
        env::remove_var("SECRET_3");
    }

    #[test]
    fn test_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        env::set_var("CONCURRENT_SECRET", "concurrent_value");

        let manager = Arc::new(EnvSecretsManager::new());
        let mut handles = vec![];

        // Spawn multiple threads accessing the same secret
        for _ in 0..10 {
            let manager_clone = Arc::clone(&manager);
            let handle = thread::spawn(move || {
                let result = manager_clone.get_secret("CONCURRENT_SECRET");
                assert!(result.is_ok());
                assert_eq!(result.unwrap(), "concurrent_value");
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Clean up
        env::remove_var("CONCURRENT_SECRET");
    }
}

/// Part D regression tests: fleet-wide rotation coordination.
#[cfg(test)]
mod rotation_tests {
    use super::*;

    /// A caller presenting the old secret must be accepted during the grace
    /// period and rejected once it elapses — the bound the coordination fix
    /// exists to make actually hold fleet-wide (see the module-level doc on
    /// `REFRESH_INTERVAL` for why two independently-clocked instances could
    /// previously double this window).
    #[test]
    fn verify_rejects_previous_value_once_grace_period_elapses() {
        let mut secret = RotatingSecret::new("v1".to_string());
        assert_eq!(secret.verify("v1"), Some(true));
        assert_eq!(secret.verify("v0"), None);

        secret.rotate("v2".to_string());
        assert_eq!(secret.verify("v2"), Some(true));
        assert_eq!(
            secret.verify("v1"),
            Some(false),
            "previous value should still verify within the grace period"
        );

        // Back-date the rotation timestamp instead of sleeping for real —
        // simulates the grace period having elapsed.
        secret.previous = secret.previous.take().map(|(v, _)| {
            (
                v,
                Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
            )
        });

        assert_eq!(
            secret.verify("v1"),
            None,
            "old secret must stop being accepted once the grace period has elapsed"
        );
        assert_eq!(secret.verify("v2"), Some(true));
    }

    /// Simulates two fleet instances whose `RotatingSecret` state is
    /// coordinated (both rotate to the same new value within milliseconds of
    /// each other via pub/sub, not up to `REFRESH_INTERVAL` apart) and
    /// confirms the old secret is rejected on *both* once the shared grace
    /// period elapses — the previously-unbounded fleet-wide window this
    /// fixes is exactly the gap between independently-clocked instances.
    #[test]
    fn staggered_instances_both_reject_stale_secret_after_shared_grace_period() {
        let mut instance_a = RotatingSecret::new("old-secret".to_string());
        let mut instance_b = RotatingSecret::new("old-secret".to_string());

        // Instance A detects the rotation (e.g. via its own poll)...
        instance_a.rotate("new-secret".to_string());
        // ...and instance B applies the same rotation shortly after, via the
        // pub/sub notification A's poll loop announces — not on B's own
        // independent REFRESH_INTERVAL clock.
        instance_b.rotate("new-secret".to_string());

        for instance in [&instance_a, &instance_b] {
            assert_eq!(instance.verify("new-secret"), Some(true));
            assert_eq!(
                instance.verify("old-secret"),
                Some(false),
                "old secret should still verify during the shared grace period"
            );
        }

        for instance in [&mut instance_a, &mut instance_b] {
            instance.previous = instance.previous.take().map(|(v, _)| {
                (
                    v,
                    Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
                )
            });
        }

        for instance in [&instance_a, &instance_b] {
            assert_eq!(
                instance.verify("old-secret"),
                None,
                "old secret must be rejected on every instance once the shared \
                 grace period elapses, not just the one that detected it first"
            );
        }
    }

    /// Task item 3: the pub/sub connect helper must degrade to `None`
    /// (fallback to poll-only) rather than error/panic when Redis is
    /// unreachable.
    #[tokio::test]
    async fn connect_rotation_pubsub_falls_back_gracefully_when_redis_unreachable() {
        let result =
            SecretsManager::connect_rotation_pubsub("redis://invalid-host-xyz-12345:6379").await;
        assert!(
            result.is_none(),
            "pub/sub connect must return None, not panic or hang, when Redis is unreachable"
        );
    }

    /// Real end-to-end roundtrip through Redis: a message published to
    /// `ROTATION_CHANNEL` from one connection is received by a subscriber
    /// created via `connect_rotation_pubsub`, with the publish timestamp
    /// intact (the same value `start_refresh_task` uses to compute
    /// `secrets_rotation_detection_lag_ms`).
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn rotation_pubsub_roundtrip() {
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

        let mut pubsub = match SecretsManager::connect_rotation_pubsub(&redis_url).await {
            Some(p) => p,
            None => {
                println!("Skipping: Redis not available");
                return;
            }
        };

        let publisher = redis::Client::open(redis_url.as_str()).unwrap();
        let mut publish_conn = publisher.get_async_connection().await.unwrap();

        // Give the SUBSCRIBE a moment to be registered server-side before publishing.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let published_at_ms = chrono::Utc::now().timestamp_millis();
        let _: i64 = redis::cmd("PUBLISH")
            .arg(ROTATION_CHANNEL)
            .arg(published_at_ms.to_string())
            .query_async(&mut publish_conn)
            .await
            .unwrap();

        let mut stream = pubsub.on_message();
        let msg = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("should receive the published rotation notification within 5s")
            .expect("stream should yield a message, not end");
        let payload: String = msg.get_payload().unwrap();
        assert_eq!(
            payload.parse::<i64>().unwrap(),
            published_at_ms,
            "received payload should be the exact publish timestamp used for lag calculation"
        );
    }

    #[tokio::test]
    async fn webhook_endpoint_secret_rotation_workflow() {
        let store = SecretsStore::new(
            "initial-webhook-secret".to_string(),
            "initial-admin-key".to_string(),
        );

        let initial_secrets = store.valid_webhook_secrets().await;
        assert_eq!(initial_secrets.len(), 1);
        assert_eq!(initial_secrets[0], "initial-webhook-secret");
    }

    #[tokio::test]
    async fn webhook_secret_rotation_and_grace_period() {
        let store = SecretsStore::new(
            "webhook-v1".to_string(),
            "admin-v1".to_string(),
        );

        assert!(store.verify_webhook_secret("webhook-v1").await);
        assert!(!store.verify_webhook_secret("webhook-v2").await);

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-v2".to_string());
        drop(secret);

        let valid_secrets = store.valid_webhook_secrets().await;
        assert_eq!(valid_secrets.len(), 2);
        assert!(valid_secrets.contains(&"webhook-v2".to_string()));
        assert!(valid_secrets.contains(&"webhook-v1".to_string()));

        assert!(store.verify_webhook_secret("webhook-v1").await);
        assert!(store.verify_webhook_secret("webhook-v2").await);
    }

    #[tokio::test]
    async fn webhook_secret_expires_after_grace_period() {
        let store = SecretsStore::new(
            "webhook-old".to_string(),
            "admin-key".to_string(),
        );

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-new".to_string());

        secret.previous = secret.previous.take().map(|(v, _)| {
            (
                v,
                Instant::now() - ROTATION_GRACE_PERIOD - Duration::from_secs(1),
            )
        });
        drop(secret);

        assert!(!store.verify_webhook_secret("webhook-old").await);
        assert!(store.verify_webhook_secret("webhook-new").await);
    }

    #[tokio::test]
    async fn multiple_webhook_secret_rotations() {
        let store = SecretsStore::new(
            "webhook-v1".to_string(),
            "admin-v1".to_string(),
        );

        let mut secret = store.anchor_webhook_secret.write().await;
        secret.rotate("webhook-v2".to_string());
        secret.rotate("webhook-v3".to_string());
        drop(secret);

        assert!(store.verify_webhook_secret("webhook-v3").await);
        assert!(!store.verify_webhook_secret("webhook-v1").await);
    }

    #[tokio::test]
    async fn secrets_store_thread_safe_webhook_rotation() {
        let store = SecretsStore::new(
            "webhook-initial".to_string(),
            "admin-initial".to_string(),
        );

        let store_clone = store.clone();
        let handle = tokio::spawn(async move {
            let mut secret = store_clone.anchor_webhook_secret.write().await;
            secret.rotate("webhook-updated".to_string());
        });

        handle.await.unwrap();

        let updated_secrets = store.valid_webhook_secrets().await;
        assert!(updated_secrets.contains(&"webhook-updated".to_string()));
    }
}
