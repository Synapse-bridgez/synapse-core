//! Secrets-provider abstraction for swap-engine credentials.
//!
//! Swap consumers read credentials through [`SwapSecrets`] rather than
//! `std::env`. Secrets are versioned values cached with a short TTL; a
//! rotation publishes the new version on a watch channel so adapters rebuild
//! their client handles (in-flight operations keep the `Arc` of the old
//! version). A failed refresh keeps the previous credential and is counted.
//!
//! Remaining direct env reads (outside swap scope, tracked for follow-up):
//! `DATABASE_URL`, `REDIS_URL` bootstrap in `src/config.rs`, and the
//! webhook/admin secrets managed by `crate::secrets`.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// A secret value at a specific version. `Debug` never prints the value.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue {
    pub version: String,
    value: Arc<str>,
}

impl SecretValue {
    pub fn new(version: impl Into<String>, value: impl AsRef<str>) -> Self {
        Self {
            version: version.into(),
            value: Arc::from(value.as_ref()),
        }
    }
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretValue(version={}, <redacted>)", self.version)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret `{0}` not found")]
    NotFound(String),
    #[error("secret backend error: {0}")]
    Backend(String),
    #[error("env fallback is not permitted in production")]
    FallbackForbidden,
}

#[async_trait]
pub trait SecretProvider: Send + Sync {
    async fn fetch(&self, name: &str) -> Result<SecretValue, SecretError>;
}

/// Development-only provider reading `std::env`. Construction fails in production.
pub struct EnvFallbackProvider;

impl EnvFallbackProvider {
    pub fn new(production: bool) -> Result<Self, SecretError> {
        if production {
            return Err(SecretError::FallbackForbidden);
        }
        Ok(Self)
    }
}

#[async_trait]
impl SecretProvider for EnvFallbackProvider {
    async fn fetch(&self, name: &str) -> Result<SecretValue, SecretError> {
        std::env::var(name)
            .map(|v| SecretValue::new("env", v))
            .map_err(|_| SecretError::NotFound(name.to_string()))
    }
}

/// Vault-style KV v2 HTTP store provider (`GET {addr}/v1/{mount}/data/{name}`).
pub struct VaultHttpProvider {
    client: reqwest::Client,
    addr: String,
    mount: String,
    token: SecretValue,
}

impl VaultHttpProvider {
    pub fn new(addr: impl Into<String>, mount: impl Into<String>, token: SecretValue) -> Self {
        Self {
            client: reqwest::Client::new(),
            addr: addr.into(),
            mount: mount.into(),
            token,
        }
    }
}

#[async_trait]
impl SecretProvider for VaultHttpProvider {
    async fn fetch(&self, name: &str) -> Result<SecretValue, SecretError> {
        let url = format!(
            "{}/v1/{}/data/{}",
            self.addr.trim_end_matches('/'),
            self.mount,
            name
        );
        let resp = self
            .client
            .get(url)
            .header("X-Vault-Token", self.token.expose())
            .send()
            .await
            .map_err(|e| SecretError::Backend(e.without_url().to_string()))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(SecretError::NotFound(name.to_string()));
        }
        let body: serde_json::Value = resp
            .error_for_status()
            .map_err(|e| SecretError::Backend(e.without_url().to_string()))?
            .json()
            .await
            .map_err(|e| SecretError::Backend(e.to_string()))?;
        let data = &body["data"];
        let value = data["data"]["value"]
            .as_str()
            .ok_or_else(|| SecretError::NotFound(name.to_string()))?;
        let version = data["metadata"]["version"].to_string();
        Ok(SecretValue::new(version, value))
    }
}

struct Entry {
    value: SecretValue,
    fetched: Instant,
    tx: watch::Sender<SecretValue>,
}

/// Cached, watchable view over a [`SecretProvider`].
pub struct SwapSecrets {
    provider: Arc<dyn SecretProvider>,
    ttl: Duration,
    cache: RwLock<HashMap<String, Entry>>,
    pub rotations: AtomicU64,
    pub rotation_failures: AtomicU64,
}

impl SwapSecrets {
    pub fn new(provider: Arc<dyn SecretProvider>, ttl: Duration) -> Self {
        Self {
            provider,
            ttl,
            cache: RwLock::new(HashMap::new()),
            rotations: AtomicU64::new(0),
            rotation_failures: AtomicU64::new(0),
        }
    }

    /// Fetch through the TTL cache.
    pub async fn get(&self, name: &str) -> Result<SecretValue, SecretError> {
        if let Some(e) = self.cache.read().unwrap().get(name) {
            if e.fetched.elapsed() < self.ttl {
                return Ok(e.value.clone());
            }
        }
        self.refresh(name).await
    }

    /// Subscribe to version changes; the receiver starts at the current value.
    pub async fn watch(&self, name: &str) -> Result<watch::Receiver<SecretValue>, SecretError> {
        self.get(name).await?;
        Ok(self.cache.read().unwrap()[name].tx.subscribe())
    }

    /// Force a refresh. On backend failure the previous value stays in use,
    /// the failure is counted and logged for alerting.
    pub async fn refresh(&self, name: &str) -> Result<SecretValue, SecretError> {
        match self.provider.fetch(name).await {
            Ok(new) => {
                let mut cache = self.cache.write().unwrap();
                match cache.get_mut(name) {
                    Some(e) => {
                        if e.value.version != new.version {
                            self.rotations.fetch_add(1, Ordering::Relaxed);
                            tracing::info!(
                                target: "audit",
                                secret = name,
                                old_version = %e.value.version,
                                new_version = %new.version,
                                "swap secret rotated"
                            );
                            e.value = new.clone();
                            let _ = e.tx.send(new.clone());
                        }
                        e.fetched = Instant::now();
                    }
                    None => {
                        let (tx, _) = watch::channel(new.clone());
                        cache.insert(
                            name.to_string(),
                            Entry {
                                value: new.clone(),
                                fetched: Instant::now(),
                                tx,
                            },
                        );
                    }
                }
                Ok(new)
            }
            Err(err) => {
                if let Some(e) = self.cache.read().unwrap().get(name) {
                    self.rotation_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(secret = name, error = %err, "swap secret rotation failed; keeping previous credential");
                    return Ok(e.value.clone());
                }
                Err(err)
            }
        }
    }

    /// Startup preflight: every required secret must resolve, else fail fast.
    pub async fn preflight(&self, required: &[&str]) -> Result<(), SecretError> {
        for name in required {
            self.get(name).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake(Mutex<HashMap<String, Result<SecretValue, String>>>);
    impl Fake {
        fn set(&self, k: &str, v: &str, ver: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(k.into(), Ok(SecretValue::new(ver, v)));
        }
        fn fail(&self, k: &str) {
            self.0.lock().unwrap().insert(k.into(), Err("down".into()));
        }
    }
    #[async_trait]
    impl SecretProvider for Fake {
        async fn fetch(&self, name: &str) -> Result<SecretValue, SecretError> {
            match self.0.lock().unwrap().get(name) {
                Some(Ok(v)) => Ok(v.clone()),
                Some(Err(e)) => Err(SecretError::Backend(e.clone())),
                None => Err(SecretError::NotFound(name.into())),
            }
        }
    }

    #[tokio::test]
    async fn caches_and_rotates_without_restart() {
        let fake = Arc::new(Fake::default());
        fake.set("lp", "old", "1");
        let s = SwapSecrets::new(fake.clone(), Duration::from_secs(60));
        let mut rx = s.watch("lp").await.unwrap();
        assert_eq!(rx.borrow().expose(), "old");
        fake.set("lp", "new", "2");
        assert_eq!(s.get("lp").await.unwrap().expose(), "old"); // cached
        assert_eq!(s.refresh("lp").await.unwrap().expose(), "new");
        rx.changed().await.unwrap();
        assert_eq!(rx.borrow().expose(), "new");
        assert_eq!(s.rotations.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn failed_rotation_keeps_previous() {
        let fake = Arc::new(Fake::default());
        fake.set("lp", "old", "1");
        let s = SwapSecrets::new(fake.clone(), Duration::from_secs(0));
        s.get("lp").await.unwrap();
        fake.fail("lp");
        assert_eq!(s.get("lp").await.unwrap().expose(), "old");
        assert_eq!(s.rotation_failures.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn preflight_fails_on_missing() {
        let s = SwapSecrets::new(Arc::new(Fake::default()), Duration::from_secs(1));
        assert!(s.preflight(&["missing"]).await.is_err());
    }

    #[test]
    fn env_fallback_forbidden_in_production() {
        assert!(matches!(
            EnvFallbackProvider::new(true),
            Err(SecretError::FallbackForbidden)
        ));
        assert!(EnvFallbackProvider::new(false).is_ok());
    }

    #[test]
    fn debug_redacts_value() {
        assert!(!format!("{:?}", SecretValue::new("1", "hunter2")).contains("hunter2"));
    }
}
