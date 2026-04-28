use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use vaultrs::auth::approle;
use vaultrs::client::{Client, VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

/// Grace period during which the previous secret remains valid after rotation.
const ROTATION_GRACE_PERIOD: Duration = Duration::from_secs(300);
/// How often to poll Vault for updated secrets.
const REFRESH_INTERVAL: Duration = Duration::from_secs(300);

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

    /// Spawn a background task that refreshes secrets from Vault every 5 minutes.
    /// Rotated secrets remain valid for a grace period so in-flight requests are not rejected.
    pub fn start_refresh_task(self, store: SecretsStore) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(REFRESH_INTERVAL);
            interval.tick().await; // skip the immediate first tick
            loop {
                interval.tick().await;
                tracing::info!("secrets_rotation: refreshing secrets from Vault");

                match self.get_anchor_secret().await {
                    Ok(new_secret) => {
                        let mut lock = store.anchor_webhook_secret.write().await;
                        if lock.current != new_secret {
                            lock.rotate(new_secret);
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
            }
        });
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
