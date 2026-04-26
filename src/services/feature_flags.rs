use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashMap;

#[derive(Clone)]
pub struct FeatureFlagService {
    pool: PgPool,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub rollout_percentage: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct FlagAuditEntry {
    pub id: uuid::Uuid,
    pub flag_name: String,
    pub old_value: Option<serde_json::Value>,
    pub new_value: Option<serde_json::Value>,
    pub actor: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl FeatureFlagService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn is_enabled(&self, flag_name: &str) -> Result<bool, sqlx::Error> {
        let result =
            sqlx::query_scalar::<_, bool>("SELECT enabled FROM feature_flags WHERE name = $1")
                .bind(flag_name)
                .fetch_optional(&self.pool)
                .await?;

        Ok(result.unwrap_or(false))
    }

    pub async fn is_enabled_for_tenant(
        &self,
        flag_name: &str,
        tenant_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let flag = sqlx::query_as::<_, (bool, Option<i32>)>(
            "SELECT enabled, rollout_percentage FROM feature_flags WHERE name = $1",
        )
        .bind(flag_name)
        .fetch_optional(&self.pool)
        .await?;

        match flag {
            None => Ok(false),
            Some((enabled, rollout_percentage)) => {
                if !enabled {
                    return Ok(false);
                }

                if let Some(percentage) = rollout_percentage {
                    if percentage < 100 {
                        let hash = Self::hash_tenant_flag(tenant_id, flag_name);
                        return Ok((hash % 100) < (percentage as u64));
                    }
                }

                Ok(true)
            }
        }
    }

    fn hash_tenant_flag(tenant_id: &str, flag_name: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        tenant_id.hash(&mut hasher);
        flag_name.hash(&mut hasher);
        hasher.finish()
    }

    pub async fn get_all_flags(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        let flags = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage FROM feature_flags",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(flags.into_iter().map(|f| (f.name, f.enabled)).collect())
    }

    pub async fn get_all(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        self.get_all_flags().await
    }

    pub async fn update(&self, name: &str, enabled: bool) -> Result<FeatureFlag, sqlx::Error> {
        let old_flag = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage FROM feature_flags WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE feature_flags SET enabled = $2 WHERE name = $1 RETURNING name, enabled, description, rollout_percentage",
        )
        .bind(name)
        .bind(enabled)
        .fetch_one(&self.pool)
        .await?;

        // Log the change
        if let Some(old) = old_flag {
            let _ = sqlx::query(
                r#"
                INSERT INTO feature_flag_audit_logs (flag_name, old_value, new_value, actor)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(name)
            .bind(json!({ "enabled": old.enabled }))
            .bind(json!({ "enabled": enabled }))
            .bind("admin")
            .execute(&self.pool)
            .await;
        }

        Ok(flag)
    }

    pub async fn update_rollout_percentage(
        &self,
        name: &str,
        percentage: i32,
    ) -> Result<FeatureFlag, sqlx::Error> {
        let old_flag = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage FROM feature_flags WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE feature_flags SET rollout_percentage = $2 WHERE name = $1 RETURNING name, enabled, description, rollout_percentage",
        )
        .bind(name)
        .bind(percentage)
        .fetch_one(&self.pool)
        .await?;

        // Log the change
        if let Some(old) = old_flag {
            let _ = sqlx::query(
                r#"
                INSERT INTO feature_flag_audit_logs (flag_name, old_value, new_value, actor)
                VALUES ($1, $2, $3, $4)
                "#,
            )
            .bind(name)
            .bind(json!({ "rollout_percentage": old.rollout_percentage }))
            .bind(json!({ "rollout_percentage": percentage }))
            .bind("admin")
            .execute(&self.pool)
            .await;
        }

        Ok(flag)
    }

    pub async fn get_audit_history(
        &self,
        flag_name: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<FlagAuditEntry>, sqlx::Error> {
        if let Some(name) = flag_name {
            sqlx::query_as::<_, FlagAuditEntry>(
                r#"
                SELECT id, flag_name, old_value, new_value, actor, timestamp
                FROM feature_flag_audit_logs
                WHERE flag_name = $1
                ORDER BY timestamp DESC
                LIMIT $2 OFFSET $3
                "#,
            )
            .bind(name)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query_as::<_, FlagAuditEntry>(
                r#"
                SELECT id, flag_name, old_value, new_value, actor, timestamp
                FROM feature_flag_audit_logs
                ORDER BY timestamp DESC
                LIMIT $1 OFFSET $2
                "#,
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_consistency() {
        let tenant_id = "tenant-123";
        let flag_name = "test_flag";

        let hash1 = FeatureFlagService::hash_tenant_flag(tenant_id, flag_name);
        let hash2 = FeatureFlagService::hash_tenant_flag(tenant_id, flag_name);

        assert_eq!(hash1, hash2, "Same tenant and flag should produce same hash");
    }

    #[test]
    fn test_percentage_distribution() {
        let flag_name = "test_flag";
        let percentage = 50;

        let mut activated = 0;
        for i in 0..100 {
            let tenant_id = format!("tenant-{}", i);
            let hash = FeatureFlagService::hash_tenant_flag(&tenant_id, flag_name);
            if (hash % 100) < (percentage as u64) {
                activated += 1;
            }
        }

        // Should be approximately 50% (allow 10% variance)
        assert!(
            activated >= 40 && activated <= 60,
            "Expected ~50% activation, got {}%",
            activated
        );
    }

    #[test]
    fn test_different_tenants_different_results() {
        let flag_name = "test_flag";
        let hash1 = FeatureFlagService::hash_tenant_flag("tenant-1", flag_name);
        let hash2 = FeatureFlagService::hash_tenant_flag("tenant-2", flag_name);

        assert_ne!(hash1, hash2, "Different tenants should produce different hashes");
    }
}
