use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CanaryRelease {
    pub release_name: String,
    pub canary_traffic_percentage: i32,
    pub flag_name: String,
    pub flag_rollout_percentage: i32,
    pub traffic_rollback_threshold: f64,
    pub flag_rollback_threshold: f64,
    pub traffic_error_rate: f64,
    pub flag_error_rate: f64,
    pub traffic_rollback_active: bool,
    pub flag_rollback_active: bool,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub updated_by: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CanaryDimension {
    Traffic,
    Flag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanaryUpdate {
    pub release_name: String,
    pub dimension: CanaryDimension,
    pub percentage: i32,
    pub actor: String,
}

#[derive(Clone)]
pub struct CanaryController {
    pool: PgPool,
}

impl CanaryController {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn status(&self, release_name: &str) -> Result<CanaryRelease, sqlx::Error> {
        sqlx::query_as::<_, CanaryRelease>(
            "SELECT release_name, canary_traffic_percentage, flag_name, flag_rollout_percentage, \
             traffic_rollback_threshold::float8, flag_rollback_threshold::float8, \
             traffic_error_rate::float8, flag_error_rate::float8, traffic_rollback_active, \
             flag_rollback_active, updated_at, updated_by \
             FROM canary_release_state WHERE release_name = $1",
        )
        .bind(release_name)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn dashboard(&self) -> Result<Vec<CanaryRelease>, sqlx::Error> {
        sqlx::query_as::<_, CanaryRelease>(
            "SELECT release_name, canary_traffic_percentage, flag_name, flag_rollout_percentage, \
             traffic_rollback_threshold::float8, flag_rollback_threshold::float8, \
             traffic_error_rate::float8, flag_error_rate::float8, traffic_rollback_active, \
             flag_rollback_active, updated_at, updated_by \
             FROM canary_release_state ORDER BY release_name",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn upsert_release(
        &self,
        release_name: &str,
        flag_name: &str,
        actor: &str,
    ) -> Result<CanaryRelease, sqlx::Error> {
        sqlx::query(
            "INSERT INTO canary_release_state (release_name, flag_name, updated_by) VALUES ($1, $2, $3) \
             ON CONFLICT (release_name) DO UPDATE SET flag_name = EXCLUDED.flag_name, updated_by = EXCLUDED.updated_by, updated_at = NOW()",
        )
        .bind(release_name)
        .bind(flag_name)
        .bind(actor)
        .execute(&self.pool)
        .await?;
        self.status(release_name).await
    }

    pub async fn set_percentage(&self, update: CanaryUpdate) -> Result<CanaryRelease, sqlx::Error> {
        if !(0..=100).contains(&update.percentage) {
            return Err(sqlx::Error::Protocol("percentage must be between 0 and 100".into()));
        }

        let (column, dimension) = match update.dimension {
            CanaryDimension::Traffic => ("canary_traffic_percentage", "traffic"),
            CanaryDimension::Flag => ("flag_rollout_percentage", "flag"),
        };
        let query = format!(
            "UPDATE canary_release_state SET {column} = $2, updated_at = NOW(), updated_by = $3 WHERE release_name = $1"
        );
        let result = sqlx::query(&query)
            .bind(&update.release_name)
            .bind(update.percentage)
            .bind(&update.actor)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }

        sqlx::query(
            "INSERT INTO canary_release_audit (release_name, dimension, old_percentage, new_percentage, action, actor) \
             VALUES ($1, $2, 0, $3, 'manual_step', $4)",
        )
        .bind(&update.release_name)
        .bind(dimension)
        .bind(update.percentage)
        .bind(&update.actor)
        .execute(&self.pool)
        .await?;
        self.status(&update.release_name).await
    }

    pub async fn record_error_rate(
        &self,
        release_name: &str,
        dimension: CanaryDimension,
        error_rate: f64,
        actor: &str,
    ) -> Result<CanaryRelease, sqlx::Error> {
        let (rate_column, threshold_column, rollback_column, percentage_column, label) = match dimension {
            CanaryDimension::Traffic => ("traffic_error_rate", "traffic_rollback_threshold", "traffic_rollback_active", "canary_traffic_percentage", "traffic"),
            CanaryDimension::Flag => ("flag_error_rate", "flag_rollback_threshold", "flag_rollback_active", "flag_rollout_percentage", "flag"),
        };
        let query = format!(
            "UPDATE canary_release_state SET {rate_column} = $2, \
             {rollback_column} = CASE WHEN $2 >= {threshold_column} THEN TRUE ELSE {rollback_column} END, \
             {percentage_column} = CASE WHEN $2 >= {threshold_column} THEN 0 ELSE {percentage_column} END, \
             updated_at = NOW(), updated_by = $3 WHERE release_name = $1 RETURNING {rollback_column}"
        );
        let rollback: bool = sqlx::query_scalar(&query)
            .bind(release_name)
            .bind(error_rate)
            .bind(actor)
            .fetch_one(&self.pool)
            .await?;
        if rollback {
            sqlx::query(
                "INSERT INTO canary_release_audit (release_name, dimension, old_percentage, new_percentage, action, error_rate, actor) \
                 VALUES ($1, $2, 0, 0, 'automatic_rollback', $3, $4)",
            )
            .bind(release_name)
            .bind(label)
            .bind(error_rate)
            .bind(actor)
            .execute(&self.pool)
            .await?;
            tracing::warn!(release_name, dimension = label, error_rate, "Canary dimension automatically rolled back");
        }
        self.status(release_name).await
    }
}
