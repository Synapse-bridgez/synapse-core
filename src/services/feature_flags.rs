use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
pub struct FeatureFlagService {
    pool: PgPool,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct FeatureFlag {
    pub name: String,
    pub enabled: bool,
    pub description: Option<String>,
    #[sqlx(default)]
    pub depends_on: Vec<String>,
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

    /// Check if a flag is enabled, recursively checking dependencies
    pub async fn is_enabled_with_deps(&self, flag_name: &str) -> Result<bool, sqlx::Error> {
        let flag = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, COALESCE(depends_on, '{}') as depends_on FROM feature_flags WHERE name = $1",
        )
        .bind(flag_name)
        .fetch_optional(&self.pool)
        .await?;

        match flag {
            None => Ok(false),
            Some(f) => {
                if !f.enabled {
                    return Ok(false);
                }
                // Check all dependencies recursively
                for dep in &f.depends_on {
                    if !self.is_enabled_with_deps(dep).await? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
        }
    }

    pub async fn get_all_flags(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        let flags = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, COALESCE(depends_on, '{}') as depends_on FROM feature_flags",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(flags.into_iter().map(|f| (f.name, f.enabled)).collect())
    }

    pub async fn get_all(&self) -> Result<HashMap<String, bool>, sqlx::Error> {
        self.get_all_flags().await
    }

    pub async fn update(&self, name: &str, enabled: bool) -> Result<FeatureFlag, sqlx::Error> {
        // If enabling, check dependencies
        if enabled {
            let flag = sqlx::query_as::<_, FeatureFlag>(
                "SELECT name, enabled, description, COALESCE(depends_on, '{}') as depends_on FROM feature_flags WHERE name = $1",
            )
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;

            if let Some(f) = flag {
                // Check all dependencies are enabled
                for dep in &f.depends_on {
                    let dep_enabled = sqlx::query_scalar::<_, bool>(
                        "SELECT enabled FROM feature_flags WHERE name = $1",
                    )
                    .bind(dep)
                    .fetch_optional(&self.pool)
                    .await?
                    .unwrap_or(false);

                    if !dep_enabled {
                        return Err(sqlx::Error::RowNotFound); // Use as error indicator
                    }
                }
            }
        } else {
            // If disabling, check if any other flags depend on this one
            let dependents: Vec<String> =
                sqlx::query_scalar("SELECT name FROM feature_flags WHERE $1 = ANY(depends_on)")
                    .bind(name)
                    .fetch_all(&self.pool)
                    .await?;

            for dependent in dependents {
                let dep_enabled = sqlx::query_scalar::<_, bool>(
                    "SELECT enabled FROM feature_flags WHERE name = $1",
                )
                .bind(&dependent)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false);

                if dep_enabled {
                    return Err(sqlx::Error::RowNotFound); // Use as error indicator
                }
            }
        }

        sqlx::query_as::<_, FeatureFlag>(
            "UPDATE feature_flags SET enabled = $2 WHERE name = $1 RETURNING name, enabled, description, COALESCE(depends_on, '{}') as depends_on",
        )
        .bind(name)
        .bind(enabled)
        .fetch_one(&self.pool)
        .await
    }

    /// Get dependency graph for visualization
    pub async fn get_dependency_graph(&self) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
        let flags = sqlx::query_as::<_, FeatureFlag>(
            "SELECT name, enabled, description, COALESCE(depends_on, '{}') as depends_on FROM feature_flags",
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(flags.into_iter().map(|f| (f.name, f.depends_on)).collect())
    }
}
