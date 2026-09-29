use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::HashMap;
use tracing::{error, info, warn};

/// Represents bloat statistics for a single table or index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BloatMetrics {
    pub object_type: String,  // 'table' or 'index'
    pub schema_name: String,
    pub object_name: String,
    pub full_name: String,    // schema.object
    pub n_dead_tup: i64,      // estimated number of dead tuples
    pub n_live_tup: i64,      // number of live tuples
    pub table_size_mb: f64,
    pub bloat_ratio: f64,     // percentage of wasted space
    pub bloat_size_mb: f64,
    pub is_partitioned: bool,
    pub partition_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutovacuumRecommendation {
    pub object_name: String,
    pub current_autovacuum_vacuum_scale_factor: f64,
    pub current_autovacuum_vacuum_threshold: i32,
    pub recommended_scale_factor: f64,
    pub recommended_threshold: i32,
    pub reason: String,
    pub estimated_recovery_mb: f64,
}

/// Table bloat monitoring job that tracks bloat on high-write tables
pub struct TableBloatMonitorJob {
    pool: PgPool,
    bloat_threshold_percent: f64,  // Alert when bloat > this percentage
    min_table_size_mb: f64,         // Only monitor tables larger than this
}

impl TableBloatMonitorJob {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            bloat_threshold_percent: 20.0,  // 20% bloat triggers monitoring
            min_table_size_mb: 10.0,          // Monitor tables > 10MB
        }
    }

    pub fn with_thresholds(
        pool: PgPool,
        bloat_threshold_percent: f64,
        min_table_size_mb: f64,
    ) -> Self {
        Self {
            pool,
            bloat_threshold_percent,
            min_table_size_mb,
        }
    }

    /// Estimate bloat for regular tables using pg_stat_user_tables
    async fn estimate_table_bloat(&self) -> Result<Vec<BloatMetrics>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (String, String, String, i64, i64, f64, i64)>(
            r#"
            SELECT
                schemaname,
                tablename,
                schemaname || '.' || tablename as full_name,
                n_dead_tup,
                n_live_tup,
                pg_total_relation_size(schemaname||'.'||tablename)::float / (1024*1024) as size_mb,
                pg_total_relation_size(schemaname||'.'||tablename) as size_bytes
            FROM pg_stat_user_tables
            WHERE pg_total_relation_size(schemaname||'.'||tablename) > $1
            ORDER BY pg_total_relation_size(schemaname||'.'||tablename) DESC
            "#
        )
        .bind(self.min_table_size_mb * 1024.0 * 1024.0)
        .fetch_all(&self.pool)
        .await?;

        let mut metrics = Vec::new();

        for (schema_name, object_name, full_name, n_dead_tup, n_live_tup, size_mb, _) in rows {
            // Calculate bloat ratio: dead_tuples / (dead + live) tuples
            let total_tuples = n_dead_tup + n_live_tup;
            let bloat_ratio = if total_tuples > 0 {
                (n_dead_tup as f64 / total_tuples as f64) * 100.0
            } else {
                0.0
            };

            // Estimate bloat size assuming ~200 bytes per row average
            let bloat_size_mb = (n_dead_tup as f64 * 200.0) / (1024.0 * 1024.0);

            // Check if table is partitioned
            let is_partitioned: bool = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM pg_partitioned_table WHERE relid = $1::regclass)"
            )
            .bind(&full_name)
            .fetch_one(&self.pool)
            .await
            .unwrap_or(false);

            let partition_count: Option<i32> = if is_partitioned {
                sqlx::query_scalar::<_, i32>(
                    "SELECT COUNT(*) FROM pg_inherits WHERE inhparent = $1::regclass"
                )
                .bind(&full_name)
                .fetch_optional(&self.pool)
                .await
                .unwrap_or(None)
            } else {
                None
            };

            metrics.push(BloatMetrics {
                object_type: "table".to_string(),
                schema_name,
                object_name,
                full_name,
                n_dead_tup,
                n_live_tup,
                table_size_mb: size_mb,
                bloat_ratio,
                bloat_size_mb,
                is_partitioned,
                partition_count,
            });
        }

        Ok(metrics)
    }

    /// Estimate bloat for indexes
    async fn estimate_index_bloat(&self) -> Result<Vec<BloatMetrics>, sqlx::Error> {
        let rows = sqlx::query_as::<_, (String, String, String, f64)>(
            r#"
            SELECT
                schemaname,
                indexname,
                schemaname || '.' || indexname as full_name,
                pg_relation_size(indexrelname::regclass)::float / (1024*1024) as size_mb
            FROM pg_stat_user_indexes
            WHERE pg_relation_size(indexrelname::regclass) > $1
            ORDER BY pg_relation_size(indexrelname::regclass) DESC
            "#
        )
        .bind(self.min_table_size_mb * 1024.0 * 1024.0)
        .fetch_all(&self.pool)
        .await?;

        let metrics = rows
            .into_iter()
            .map(|(schema_name, object_name, full_name, size_mb)| BloatMetrics {
                object_type: "index".to_string(),
                schema_name,
                object_name,
                full_name,
                n_dead_tup: 0,
                n_live_tup: 0,
                table_size_mb: size_mb,
                bloat_ratio: 0.0,  // Index bloat estimation is complex, simplified here
                bloat_size_mb: 0.0,
                is_partitioned: false,
                partition_count: None,
            })
            .collect();

        Ok(metrics)
    }

    /// Generate autovacuum tuning recommendations
    async fn generate_recommendations(
        &self,
        bloat_metrics: &[BloatMetrics],
    ) -> Result<Vec<AutovacuumRecommendation>, sqlx::Error> {
        let mut recommendations = Vec::new();

        for metric in bloat_metrics {
            if metric.object_type != "table" || metric.bloat_ratio < self.bloat_threshold_percent {
                continue;
            }

            // Get current autovacuum settings
            let current_scale_factor: f64 = sqlx::query_scalar(
                r#"
                SELECT current_setting('autovacuum_vacuum_scale_factor')::float
                "#
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0.1);

            let current_threshold: i32 = sqlx::query_scalar(
                r#"
                SELECT current_setting('autovacuum_vacuum_threshold')::int
                "#
            )
            .fetch_one(&self.pool)
            .await
            .unwrap_or(50);

            // Recommend more aggressive vacuuming
            let recommended_scale_factor = (current_scale_factor * 0.5).max(0.01);
            let recommended_threshold = (current_threshold / 2).max(10);

            let reason = format!(
                "Table has {:.1}% bloat ({:.1}MB wasted). Dead tuples: {}",
                metric.bloat_ratio, metric.bloat_size_mb, metric.n_dead_tup
            );

            recommendations.push(AutovacuumRecommendation {
                object_name: metric.full_name.clone(),
                current_autovacuum_vacuum_scale_factor: current_scale_factor,
                current_autovacuum_vacuum_threshold: current_threshold,
                recommended_scale_factor,
                recommended_threshold,
                reason,
                estimated_recovery_mb: metric.bloat_size_mb,
            });
        }

        Ok(recommendations)
    }

    /// Record bloat metrics to the metrics system
    async fn emit_bloat_metrics(&self, metrics: &[BloatMetrics]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for metric in metrics {
            crate::metrics::table_bloat_ratio().record(
                metric.bloat_ratio,
                &[
                    opentelemetry::KeyValue::new("schema", metric.schema_name.clone()),
                    opentelemetry::KeyValue::new("table", metric.object_name.clone()),
                    opentelemetry::KeyValue::new("object_type", metric.object_type.clone()),
                ],
            );

            crate::metrics::table_bloat_size_mb().record(
                metric.bloat_size_mb,
                &[
                    opentelemetry::KeyValue::new("schema", metric.schema_name.clone()),
                    opentelemetry::KeyValue::new("table", metric.object_name.clone()),
                ],
            );

            if metric.bloat_ratio > self.bloat_threshold_percent {
                info!(
                    schema = %metric.schema_name,
                    table = %metric.object_name,
                    bloat_ratio = metric.bloat_ratio,
                    bloat_mb = metric.bloat_size_mb,
                    "High bloat detected in table"
                );
            }
        }

        Ok(())
    }
}

#[async_trait]
impl crate::services::scheduler::Job for TableBloatMonitorJob {
    fn name(&self) -> &str {
        "table_bloat_monitor"
    }

    fn schedule(&self) -> &str {
        "0 */6 * * * * *"  // Every 6 hours
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting table bloat monitoring");

        // Estimate table bloat
        let table_metrics = match self.estimate_table_bloat().await {
            Ok(metrics) => metrics,
            Err(e) => {
                error!("Failed to estimate table bloat: {}", e);
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Table bloat estimation failed: {}", e),
                )));
            }
        };

        info!("Scanned {} tables for bloat", table_metrics.len());

        // Emit metrics
        if let Err(e) = self.emit_bloat_metrics(&table_metrics).await {
            warn!("Failed to emit bloat metrics: {}", e);
        }

        // Generate recommendations for bloated tables
        let recommendations = match self.generate_recommendations(&table_metrics).await {
            Ok(recs) => recs,
            Err(e) => {
                warn!("Failed to generate recommendations: {}", e);
                Vec::new()
            }
        };

        if !recommendations.is_empty() {
            info!("Generated {} autovacuum recommendations", recommendations.len());
            for rec in &recommendations {
                info!(
                    table = %rec.object_name,
                    current_scale = rec.current_autovacuum_vacuum_scale_factor,
                    recommended_scale = rec.recommended_scale_factor,
                    recovery_mb = rec.estimated_recovery_mb,
                    "Autovacuum tuning recommendation: {}",
                    rec.reason
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloat_ratio_calculation() {
        let dead_tuples = 1000i64;
        let live_tuples = 4000i64;
        let total = dead_tuples + live_tuples;
        let bloat_ratio = (dead_tuples as f64 / total as f64) * 100.0;

        assert_eq!(bloat_ratio, 20.0);
    }

    #[test]
    fn test_bloat_size_estimation() {
        let dead_tuples = 10000i64;
        let bytes_per_row = 200.0;
        let bloat_mb = (dead_tuples as f64 * bytes_per_row) / (1024.0 * 1024.0);

        assert!((bloat_mb - 1.9).abs() < 0.1); // ~1.9MB
    }

    #[test]
    fn test_autovacuum_recommendation_scaling() {
        let current_scale = 0.1;
        let recommended = (current_scale * 0.5).max(0.01);

        assert_eq!(recommended, 0.05);
    }

    #[test]
    fn test_threshold_recommendation() {
        let current_threshold = 50;
        let recommended = (current_threshold / 2).max(10);

        assert_eq!(recommended, 25);
    }

    #[test]
    fn test_threshold_minimum_boundary() {
        let current_threshold = 15;
        let recommended = (current_threshold / 2).max(10);

        assert_eq!(recommended, 10); // Respects minimum
    }
}
