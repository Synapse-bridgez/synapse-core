use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// How often the in-memory flag snapshot is refreshed from the database.
/// Flag changes made directly in the database become visible within this
/// window; changes made through this service refresh the snapshot eagerly.
pub const FLAG_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct FeatureFlagService {
    pool: PgPool,
    snapshot: Arc<RwLock<HashMap<String, FlagSnapshotEntry>>>,
}

#[derive(Debug, Clone)]
struct FlagSnapshotEntry {
    enabled: bool,
    rollout_percentage: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
    pub rollout_percentage: Option<i32>,
    pub depends_on: Vec<String>,
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
        Self {
            pool,
            snapshot: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Loads the full flag set into the in-memory snapshot. Call once at
    /// startup and then periodically (see [`Self::spawn_refresh_task`]).
    pub async fn refresh(&self) -> Result<(), sqlx::Error> {
        let flags = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage, COALESCE(depends_on, '{}') as depends_on FROM feature_flags",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut snapshot = self.snapshot.write().await;
        *snapshot = flags
            .into_iter()
            .map(|f| {
                (
                    f.name,
                    FlagSnapshotEntry {
                        enabled: f.enabled,
                        rollout_percentage: f.rollout_percentage,
                    },
                )
            })
            .collect();
        Ok(())
    }

    /// Spawns a background task that refreshes the snapshot every
    /// [`FLAG_REFRESH_INTERVAL`]. Errors are logged and the previous snapshot
    /// is retained so a transient DB failure does not disable all flags.
    pub fn spawn_refresh_task(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(FLAG_REFRESH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(err) = service.refresh().await {
                    tracing::warn!(error = %err, "feature flag snapshot refresh failed");
                }
            }
        });
    }

    /// Checks whether a flag is enabled, **ignoring `rollout_percentage`
    /// entirely**. If the flag has a `rollout_percentage` configured, an
    /// operator's attempt to limit blast radius to X% is silently not
    /// applied here — every caller sees the flag as fully on. Only use this
    /// for flags that genuinely have no natural per-caller scoping key (e.g.
    /// a process-wide toggle); anything gating per-request/per-tenant/
    /// per-account behavior should use [`Self::is_enabled_for_key`] instead.
    pub async fn is_enabled_ignoring_rollout(&self, flag_name: &str) -> Result<bool, sqlx::Error> {
        let snapshot = self.snapshot.read().await;
        Ok(snapshot.get(flag_name).map(|f| f.enabled).unwrap_or(false))
    }

    /// Checks whether a flag is enabled for a given tenant, correctly
    /// applying `rollout_percentage` via a deterministic per-tenant hash.
    pub async fn is_enabled_for_tenant(
        &self,
        flag_name: &str,
        tenant_id: &str,
    ) -> Result<bool, sqlx::Error> {
        self.is_enabled_for_key(flag_name, tenant_id).await
    }

    /// Checks whether a flag is enabled for an arbitrary stable scope key
    /// (tenant id, stellar account, etc.), correctly applying
    /// `rollout_percentage`: if the flag is enabled and `rollout_percentage`
    /// is set below 100, `key` is hashed together with `flag_name` and only
    /// included if the hash falls within the configured percentage — the
    /// same key always gets the same answer for a given flag, so a gradual
    /// rollout is stable across repeated calls instead of coin-flipping
    /// per call.
    pub async fn is_enabled_for_key(
        &self,
        flag_name: &str,
        key: &str,
    ) -> Result<bool, sqlx::Error> {
        let snapshot = self.snapshot.read().await;

        match snapshot.get(flag_name) {
            None => Ok(false),
            Some(entry) => {
                if !entry.enabled {
                    return Ok(false);
                }

                if let Some(percentage) = entry.rollout_percentage {
                    if percentage < 100 {
                        let hash = Self::hash_key_flag(key, flag_name);
                        return Ok((hash % 100) < (percentage as u64));
                    }
                }

                Ok(true)
            }
        }
    }

    fn hash_key_flag(key: &str, flag_name: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        flag_name.hash(&mut hasher);
        hasher.finish()
    }

    pub async fn get_all_flags(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        let snapshot = self.snapshot.read().await;
        Ok(snapshot.iter().map(|(k, v)| (k.clone(), v.enabled)).collect())
    }

    pub async fn get_all(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        self.get_all_flags().await
    }

    pub async fn update(&self, name: &str, enabled: bool) -> Result<FeatureFlag, sqlx::Error> {
        let old_flag = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage, COALESCE(depends_on, '{}') as depends_on FROM feature_flags WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE feature_flags SET enabled = $2 WHERE name = $1 RETURNING name, enabled, description, rollout_percentage, COALESCE(depends_on, '{}') as depends_on",
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

        // Keep the snapshot in sync so the change is visible immediately.
        let _ = self.refresh().await;

        Ok(flag)
    }

    pub async fn update_rollout_percentage(
        &self,
        name: &str,
        percentage: i32,
    ) -> Result<FeatureFlag, sqlx::Error> {
        let old_flag = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, rollout_percentage, COALESCE(depends_on, '{}') as depends_on FROM feature_flags WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE feature_flags SET rollout_percentage = $2 WHERE name = $1 RETURNING name, enabled, description, rollout_percentage, COALESCE(depends_on, '{}') as depends_on",
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

        // Keep the snapshot in sync so the change is visible immediately.
        let _ = self.refresh().await;

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
