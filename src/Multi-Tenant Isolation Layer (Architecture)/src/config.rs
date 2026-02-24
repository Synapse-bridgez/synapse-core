use crate::tenant::TenantConfig;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            database_url: std::env::var("DATABASE_URL")?,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub tenant_configs: Arc<tokio::sync::RwLock<HashMap<Uuid, TenantConfig>>>,
}

impl AppState {
    pub fn new(pool: PgPool, _config: Config) -> Self {
        Self {
            pool,
            tenant_configs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        }
    }
    
    pub async fn load_tenant_configs(&self) -> crate::error::Result<()> {
        let configs = crate::db::queries::get_all_tenant_configs(&self.pool).await?;
        let mut map = self.tenant_configs.write().await;
        map.clear();
        for config in configs {
            map.insert(config.tenant_id, config);
        }
        Ok(())
    }
    
    pub async fn get_tenant_config(&self, tenant_id: Uuid) -> Option<TenantConfig> {
        self.tenant_configs.read().await.get(&tenant_id).cloned()
    }
}
