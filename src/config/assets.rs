use arc_swap::ArcSwap;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

use crate::db::models::Asset;

pub struct AssetCache {
    inner: ArcSwap<HashMap<String, Asset>>,
}

impl AssetCache {
    pub async fn start(pool: PgPool, refresh_interval: Duration) -> Arc<Self> {
        let initial_vec = Asset::fetch_all(&pool).await.unwrap_or_default();
        let mut map: HashMap<String, Asset> = HashMap::new();
        for a in initial_vec {
            map.insert(a.asset_code.clone(), a);
        }
        let arc_map = Arc::new(map);

        let cache = Arc::new(AssetCache {
            inner: ArcSwap::from(arc_map.clone()),
        });

        // spawn background refresher
        let cache_clone = cache.clone();
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            loop {
                sleep(refresh_interval).await;
                if let Ok(new_assets) = Asset::fetch_all(&pool_clone).await {
                    let mut new_map = HashMap::new();
                    for a in new_assets {
                        new_map.insert(a.asset_code.clone(), a);
                    }
                    cache_clone.inner.store(Arc::new(new_map));
                }
            }
        });

        cache
    }

    pub fn get(&self, code: &str) -> Option<Asset> {
        let arc = self.inner.load_full();
        arc.get(code).cloned()
    }

    pub async fn reload_once(&self, pool: &PgPool) -> anyhow::Result<()> {
        let new_assets = Asset::fetch_all(pool).await?;
        let mut new_map = HashMap::new();
        for a in new_assets {
            new_map.insert(a.asset_code.clone(), a);
        }
        self.inner.store(Arc::new(new_map));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn create_test_asset(code: &str, issuer: Option<String>) -> Asset {
        Asset {
            asset_code: code.to_string(),
            issuer,
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
        map.insert(
            "USD".to_string(),
            create_test_asset("USD", Some("ISSUER123".to_string())),
        );
        map.insert("EUR".to_string(), create_test_asset("EUR", None));

        let cache = AssetCache {
            inner: ArcSwap::from(Arc::new(map)),
        };

        let usd = cache.get("USD");
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
