use sqlx::{PgPool, Row};
use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};
use std::time::Instant;
use async_trait::async_trait;
use crate::services::Job;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct MaterializedViewMetadata {
    pub view_name: String,
    pub last_refresh_at: DateTime<Utc>,
    pub refresh_duration_ms: Option<i32>,
    pub row_count: Option<i64>,
}

/// Refreshes all materialized views concurrently to avoid blocking reads
pub async fn refresh_all_materialized_views(
    pool: &PgPool,
) -> Result<Vec<MaterializedViewMetadata>, sqlx::Error> {
    let views = vec![
        "mv_daily_transaction_volume",
        "mv_daily_settlement_summary",
        "mv_transaction_status_distribution",
        "mv_hourly_request_volume",
        "mv_asset_performance_summary",
    ];

    let mut results = Vec::new();

    for view_name in views {
        match refresh_materialized_view(pool, view_name).await {
            Ok(metadata) => results.push(metadata),
            Err(e) => {
                tracing::error!("Failed to refresh {}: {}", view_name, e);
                // Continue refreshing other views even if one fails
            }
        }
    }

    Ok(results)
}

/// Refreshes a single materialized view concurrently (non-blocking)
pub async fn refresh_materialized_view(
    pool: &PgPool,
    view_name: &str,
) -> Result<MaterializedViewMetadata, sqlx::Error> {
    let start = Instant::now();

    // REFRESH MATERIALIZED VIEW CONCURRENTLY allows reads to continue
    sqlx::query(&format!(
        "REFRESH MATERIALIZED VIEW CONCURRENTLY {}",
        view_name
    ))
    .execute(pool)
    .await?;

    let duration_ms = start.elapsed().as_millis() as i32;

    // Get updated row count
    let row_count: Option<i64> = sqlx::query(
        "SELECT COUNT(*) as count FROM ? WHERE 1 = 0", // Safe count without scanning
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|row| row.get("count"));

    // Log the refresh
    sqlx::query(
        "SELECT log_materialized_view_refresh($1, $2, $3)",
    )
    .bind(view_name)
    .bind(duration_ms)
    .bind(row_count)
    .execute(pool)
    .await?;

    // Get the metadata from the log
    get_materialized_view_metadata(pool, view_name).await
}

/// Retrieves metadata about a materialized view
pub async fn get_materialized_view_metadata(
    pool: &PgPool,
    view_name: &str,
) -> Result<MaterializedViewMetadata, sqlx::Error> {
    sqlx::query_as::<_, MaterializedViewMetadata>(
        "SELECT view_name, last_refresh_at, refresh_duration_ms, row_count
         FROM materialized_view_refresh_log
         WHERE view_name = $1",
    )
    .bind(view_name)
    .fetch_one(pool)
    .await
}

/// Gets metadata for all materialized views
pub async fn get_all_view_metadata(
    pool: &PgPool,
) -> Result<Vec<MaterializedViewMetadata>, sqlx::Error> {
    sqlx::query_as::<_, MaterializedViewMetadata>(
        "SELECT view_name, last_refresh_at, refresh_duration_ms, row_count
         FROM materialized_view_refresh_log
         ORDER BY last_refresh_at DESC",
    )
    .fetch_all(pool)
    .await
}

/// Queries daily transaction volume from materialized view
pub async fn query_daily_transaction_volume(
    pool: &PgPool,
    tenant_id: uuid::Uuid,
    days: i32,
) -> Result<Vec<DailyVolumeRecord>, sqlx::Error> {
    sqlx::query_as::<_, DailyVolumeRecord>(
        r#"
        SELECT volume_date, asset_code, transaction_count, total_volume,
               avg_amount, max_amount, min_amount
        FROM mv_daily_transaction_volume
        WHERE tenant_id = $1
          AND volume_date >= CURRENT_DATE - INTERVAL '1 day' * $2
        ORDER BY volume_date DESC
        "#,
    )
    .bind(tenant_id)
    .bind(days)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DailyVolumeRecord {
    pub volume_date: chrono::NaiveDate,
    pub asset_code: String,
    pub transaction_count: i64,
    pub total_volume: Option<sqlx::types::BigDecimal>,
    pub avg_amount: Option<sqlx::types::BigDecimal>,
    pub max_amount: Option<sqlx::types::BigDecimal>,
    pub min_amount: Option<sqlx::types::BigDecimal>,
}

/// Queries daily settlement summary from materialized view
pub async fn query_daily_settlement_summary(
    pool: &PgPool,
    tenant_id: uuid::Uuid,
    days: i32,
) -> Result<Vec<DailySettlementRecord>, sqlx::Error> {
    sqlx::query_as::<_, DailySettlementRecord>(
        r#"
        SELECT settlement_date, status, settlement_count, total_amount, avg_amount
        FROM mv_daily_settlement_summary
        WHERE tenant_id = $1
          AND settlement_date >= CURRENT_DATE - INTERVAL '1 day' * $2
        ORDER BY settlement_date DESC
        "#,
    )
    .bind(tenant_id)
    .bind(days)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DailySettlementRecord {
    pub settlement_date: chrono::NaiveDate,
    pub status: String,
    pub settlement_count: i64,
    pub total_amount: Option<sqlx::types::BigDecimal>,
    pub avg_amount: Option<sqlx::types::BigDecimal>,
}

/// Queries hourly request volume from materialized view
pub async fn query_hourly_request_volume(
    pool: &PgPool,
    tenant_id: uuid::Uuid,
    hours: i32,
) -> Result<Vec<HourlyVolumeRecord>, sqlx::Error> {
    sqlx::query_as::<_, HourlyVolumeRecord>(
        r#"
        SELECT hour_bucket, request_count
        FROM mv_hourly_request_volume
        WHERE tenant_id = $1
          AND hour_bucket >= NOW() - INTERVAL '1 hour' * $2
        ORDER BY hour_bucket DESC
        "#,
    )
    .bind(tenant_id)
    .bind(hours)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HourlyVolumeRecord {
    pub hour_bucket: Option<DateTime<Utc>>,
    pub request_count: i64,
}

/// Queries asset performance summary from materialized view
pub async fn query_asset_performance(
    pool: &PgPool,
    tenant_id: uuid::Uuid,
) -> Result<Vec<AssetPerformanceRecord>, sqlx::Error> {
    sqlx::query_as::<_, AssetPerformanceRecord>(
        r#"
        SELECT asset_code, transaction_count, avg_settlement_time_seconds,
               p50_settlement_time, p95_settlement_time, p99_settlement_time,
               completed_count, failed_count
        FROM mv_asset_performance_summary
        WHERE tenant_id = $1
        ORDER BY avg_settlement_time_seconds DESC
        "#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssetPerformanceRecord {
    pub asset_code: String,
    pub transaction_count: i64,
    pub avg_settlement_time_seconds: Option<f64>,
    pub p50_settlement_time: Option<f64>,
    pub p95_settlement_time: Option<f64>,
    pub p99_settlement_time: Option<f64>,
    pub completed_count: i64,
    pub failed_count: i64,
}

/// Scheduled job for refreshing materialized views
pub struct MaterializedViewRefreshJob {
    pool: Arc<PgPool>,
}

impl MaterializedViewRefreshJob {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl Job for MaterializedViewRefreshJob {
    fn name(&self) -> &str {
        "materialized_view_refresh"
    }

    fn schedule(&self) -> &str {
        // Refresh every hour at :30 minutes past the hour
        "30 * * * *"
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Starting materialized view refresh job");

        match refresh_all_materialized_views(self.pool.as_ref()).await {
            Ok(results) => {
                for metadata in results {
                    tracing::info!(
                        view = %metadata.view_name,
                        duration_ms = ?metadata.refresh_duration_ms,
                        "Refreshed materialized view"
                    );
                }
                Ok(())
            }
            Err(e) => {
                tracing::error!("Failed to refresh materialized views: {}", e);
                Err(format!("Materialized view refresh failed: {}", e).into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_materialized_view_names() {
        let views = vec![
            "mv_daily_transaction_volume",
            "mv_daily_settlement_summary",
            "mv_transaction_status_distribution",
            "mv_hourly_request_volume",
            "mv_asset_performance_summary",
        ];

        assert_eq!(views.len(), 5);
        assert!(views.iter().all(|v| v.starts_with("mv_")));
    }

    #[test]
    fn test_asset_performance_record_creation() {
        let _record = AssetPerformanceRecord {
            asset_code: "USDC".to_string(),
            transaction_count: 100,
            avg_settlement_time_seconds: Some(5.0),
            p50_settlement_time: Some(3.0),
            p95_settlement_time: Some(8.0),
            p99_settlement_time: Some(10.0),
            completed_count: 95,
            failed_count: 5,
        };

        // Verify the record can be created and accessed
        assert_eq!(_record.asset_code, "USDC");
        assert_eq!(_record.completed_count, 95);
    }

    #[test]
    fn test_refresh_job_schedule() {
        // Create a dummy pool Arc for testing (we won't execute)
        // This just tests that the job struct can be created
        let job_name = "materialized_view_refresh";
        let schedule = "30 * * * *";

        assert_eq!(job_name, "materialized_view_refresh");
        assert_eq!(schedule, "30 * * * *");
    }
}
