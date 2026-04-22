use std::collections::HashMap;
use std::env;

use anyhow::{Context, Result};
use vaultrs::auth::approle;
use vaultrs::client::{Client, VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

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
            let value = std::env::var(key).map_err(|_| format!("Secret '{}' not found", key))?;

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
