use sqlx::PgPool;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct FeatureFlagService {
    pool: PgPool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
}

impl FeatureFlagService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn is_enabled(&self, flag_name: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query_scalar!(
            "SELECT enabled FROM feature_flags WHERE name = $1",
            flag_name
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(result.unwrap_or(false))
    }

    pub async fn get_all_flags(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        let flags = sqlx::query_as!(
            FeatureFlag,
            "SELECT name, enabled, description FROM feature_flags"
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(flags.into_iter().map(|f| (f.name, f.enabled)).collect())
    }
}