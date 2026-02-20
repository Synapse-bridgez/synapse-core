use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct FeatureFlagService {
    pool: PgPool,
    cache: Arc<RwLock<HashMap<String, bool>>>,
}

impl FeatureFlagService {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn start(&self, refresh_interval_hours: u64) {
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = service.refresh_cache().await {
                    tracing::error!("Failed to refresh feature flags cache: {}", e);
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    refresh_interval_hours * 3600,
                ))
                .await;
            }
        });
    }

    pub async fn refresh_cache(&self) -> anyhow::Result<()> {
        let flags = sqlx::query_as::<_, (String, bool)>("SELECT name, enabled FROM feature_flags")
            .fetch_all(&self.pool)
            .await?;

        let mut cache = self.cache.write().await;
        cache.clear();
        for (name, enabled) in flags {
            cache.insert(name, enabled);
        }
        tracing::info!("Feature flags cache refreshed with {} flags", cache.len());
        Ok(())
    }

    pub async fn is_enabled(&self, name: &str) -> bool {
        self.cache.read().await.get(name).copied().unwrap_or(false)
    }

    pub async fn get_all(&self) -> anyhow::Result<Vec<FeatureFlag>> {
        let flags = sqlx::query_as::<_, (String, bool, Option<String>, DateTime<Utc>)>(
            "SELECT name, enabled, description, updated_at FROM feature_flags ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(name, enabled, description, updated_at)| FeatureFlag {
            name,
            enabled,
            description,
            updated_at,
        })
        .collect();

        Ok(flags)
    }

    pub async fn update(&self, name: &str, enabled: bool) -> anyhow::Result<FeatureFlag> {
        let flag = sqlx::query_as::<_, (String, bool, Option<String>, DateTime<Utc>)>(
            "UPDATE feature_flags SET enabled = $1, updated_at = NOW() WHERE name = $2 
             RETURNING name, enabled, description, updated_at",
        )
        .bind(enabled)
        .bind(name)
        .fetch_one(&self.pool)
        .await?;

        // Update cache
        self.cache.write().await.insert(name.to_string(), enabled);
        tracing::info!("Feature flag '{}' updated to {}", name, enabled);

        Ok(FeatureFlag {
            name: flag.0,
            enabled: flag.1,
            description: flag.2,
            updated_at: flag.3,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_feature_flag_creation() {
        let flag = FeatureFlag {
            name: "test_flag".to_string(),
            enabled: true,
            description: Some("Test flag".to_string()),
            updated_at: Utc::now(),
        };
        assert_eq!(flag.name, "test_flag");
        assert!(flag.enabled);
    }
}
