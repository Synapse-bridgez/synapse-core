use arc_swap::ArcSwap;
use sqlx::PgPool;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::db::models::{canonical_asset_id, Asset};

/// Why a quote or order referencing an asset was rejected. Carries the
/// canonical `{CODE}:{ISSUER}` id that was looked up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// No registry row matches this code/issuer pair.
    Unknown(String),
    /// A matching row exists but is disabled.
    Disabled(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetError::Unknown(id) => write!(f, "unknown asset: {id}"),
            AssetError::Disabled(id) => write!(f, "disabled asset: {id}"),
        }
    }
}

impl std::error::Error for AssetError {}

/// Registry of tradable assets, keyed by canonical `{CODE}:{ISSUER}` id so the
/// same code issued by two different issuers stays two distinct entries.
pub struct AssetCache {
    inner: ArcSwap<HashMap<String, Asset>>,
}

impl AssetCache {
    pub async fn start(pool: PgPool, refresh_interval: Duration) -> Arc<Self> {
        let initial_vec = match Asset::fetch_all(&pool).await {
            Ok(assets) => assets,
            Err(e) => {
                // Starts empty, so every asset is rejected until a refresh succeeds.
                tracing::error!("Asset registry initial load failed: {}", e);
                Vec::new()
            }
        };
        let map = Self::build_map(initial_vec);
        let cache = Arc::new(AssetCache {
            inner: ArcSwap::from(Arc::new(map)),
        });

        let cache_clone = cache.clone();
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            loop {
                sleep(refresh_interval).await;
                if let Err(e) = cache_clone.reload_once(&pool_clone).await {
                    tracing::error!(
                        "Asset registry refresh failed, keeping last known good: {}",
                        e
                    );
                }
            }
        });

        cache
    }

    fn build_map(assets: Vec<Asset>) -> HashMap<String, Asset> {
        assets.into_iter().map(|a| (a.canonical_id(), a)).collect()
    }

    /// Returns the asset for a canonical id if it is registered and enabled.
    pub fn get(&self, canonical_id: &str) -> Option<Asset> {
        self.lookup(canonical_id).filter(|a| a.enabled)
    }

    /// Returns the asset for a canonical id regardless of its enabled state.
    fn lookup(&self, canonical_id: &str) -> Option<Asset> {
        let arc = self.inner.load_full();
        arc.get(canonical_id).cloned()
    }

    /// Canonicalizes `code`/`issuer` and returns the asset if registered and enabled.
    pub fn get_by_code_issuer(&self, code: &str, issuer: Option<&str>) -> Option<Asset> {
        self.get(&canonical_asset_id(code, issuer))
    }

    /// Returns true if the canonical id is registered and enabled.
    pub fn is_registered(&self, canonical_id: &str) -> bool {
        self.get(canonical_id).is_some()
    }

    /// Validation hook for quotes and orders. Resolves the code/issuer pair to a
    /// registered, enabled asset or returns a typed reason it cannot be used.
    pub fn validate(&self, code: &str, issuer: Option<&str>) -> Result<Asset, AssetError> {
        let id = canonical_asset_id(code, issuer);
        match self.lookup(&id) {
            None => Err(AssetError::Unknown(id)),
            Some(asset) if !asset.enabled => Err(AssetError::Disabled(id)),
            Some(asset) => Ok(asset),
        }
    }

    /// Reloads the registry from Postgres. On failure the previous registry is
    /// left in place.
    pub async fn reload_once(&self, pool: &PgPool) -> anyhow::Result<()> {
        let new_assets = Asset::fetch_all(pool).await?;
        self.inner.store(Arc::new(Self::build_map(new_assets)));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn create_test_asset(code: &str, issuer: Option<String>) -> Asset {
        Asset {
            id: uuid::Uuid::new_v4(),
            asset_code: code.to_string(),
            asset_issuer: issuer,
            decimals: 7,
            min_amount: None,
            max_amount: None,
            metadata: None,
            enabled: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn test_asset_cache_initialization() {
        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(HashMap::new())),
        };

        assert!(cache.get("USD").is_none());
        assert!(cache.get("EUR").is_none());
    }

    #[tokio::test]
    async fn test_asset_cache_get() {
        let mut map = HashMap::new();
        let usd = create_test_asset("USD", Some("ISSUER123".to_string()));
        map.insert(usd.canonical_id(), usd);
        let eur = create_test_asset("EUR", None);
        map.insert(eur.canonical_id(), eur);

        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(map)),
        };

        let usd = cache.get("USD:ISSUER123");
        assert!(usd.is_some());
        assert_eq!(usd.unwrap().asset_code, "USD");

        let eur = cache.get("EUR");
        assert!(eur.is_some());
        assert_eq!(eur.unwrap().asset_code, "EUR");

        assert!(cache.get("GBP").is_none());
    }

    #[tokio::test]
    async fn test_asset_cache_concurrent_reads() {
        let mut map = HashMap::new();
        for i in 0..100 {
            map.insert(
                format!("ASSET{}", i),
                create_test_asset(&format!("ASSET{}", i), None),
            );
        }

        let cache = Arc::new(AssetCache {
            inner: ArcSwap::from(Arc::new(map)),
        });

        let mut handles = vec![];
        let success_count = Arc::new(AtomicUsize::new(0));

        for _ in 0..50 {
            let cache_clone = cache.clone();
            let success_clone = success_count.clone();
            let handle = tokio::spawn(async move {
                for j in 0..100 {
                    let asset_code = format!("ASSET{}", j);
                    if let Some(asset) = cache_clone.get(&asset_code) {
                        assert_eq!(asset.asset_code, asset_code);
                        success_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        assert_eq!(success_count.load(Ordering::Relaxed), 50 * 100);
    }

    #[tokio::test]
    async fn test_asset_cache_reload() {
        let mut initial_map = HashMap::new();
        initial_map.insert("USD".to_string(), create_test_asset("USD", None));

        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(initial_map)),
        };

        assert!(cache.get("USD").is_some());
        assert!(cache.get("EUR").is_none());

        let mut new_map = HashMap::new();
        new_map.insert("EUR".to_string(), create_test_asset("EUR", None));
        new_map.insert("GBP".to_string(), create_test_asset("GBP", None));

        cache.inner.store(Arc::new(new_map));

        assert!(cache.get("USD").is_none());
        assert!(cache.get("EUR").is_some());
        assert!(cache.get("GBP").is_some());
    }

    #[tokio::test]
    async fn test_asset_cache_empty() {
        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(HashMap::new())),
        };

        assert!(cache.get("").is_none());
        assert!(cache.get("NONEXISTENT").is_none());
    }

    #[tokio::test]
    async fn test_asset_cache_clone_independence() {
        let mut map = HashMap::new();
        map.insert("USD".to_string(), create_test_asset("USD", None));

        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(map)),
        };

        let asset1 = cache.get("USD").unwrap();
        let asset2 = cache.get("USD").unwrap();

        assert_eq!(asset1.asset_code, asset2.asset_code);
        assert_eq!(asset1.asset_code, "USD");
    }
}
