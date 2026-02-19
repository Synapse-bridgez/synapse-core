use arc_swap::ArcSwap;
use sqlx::PgPool;
use std::sync::Arc;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

use crate::db::models::Asset;

pub struct AssetCache {
    inner: ArcSwap<Arc<HashMap<String, Asset>>>,
}

impl AssetCache {
    pub async fn start(pool: PgPool, refresh_interval: Duration) -> Arc<Self> {
        let initial_vec = Asset::fetch_all(&pool).await.unwrap_or_default();
        let mut map: HashMap<String, Asset> = HashMap::new();
        for a in initial_vec { map.insert(a.asset_code.clone(), a); }
        let arc_map = Arc::new(map);

        let cache = Arc::new(AssetCache {
            inner: ArcSwap::from_pointee(arc_map.clone()),
        });

        // spawn background refresher
        let cache_clone = cache.clone();
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            loop {
                sleep(refresh_interval).await;
                if let Ok(new_assets) = Asset::fetch_all(&pool_clone).await {
                    let mut new_map = HashMap::new();
                    for a in new_assets { new_map.insert(a.asset_code.clone(), a); }
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
        for a in new_assets { new_map.insert(a.asset_code.clone(), a); }
        self.inner.store(Arc::new(new_map));
        Ok(())
    }
}
