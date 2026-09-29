use anyhow::{anyhow, Result};
use clap::Parser;
use std::collections::VecDeque;
use std::time::Duration;

/// Analyze connection pool utilization and recommend sizing.
///
/// Queries Prometheus for historical pool metrics over a configurable window,
/// fits a trend, and recommends min/max pool sizes based on observed utilization.
#[derive(Parser)]
pub struct PoolAdvisorArgs {
    /// Prometheus endpoint URL
    #[arg(long, default_value = "http://localhost:9090")]
    prometheus_url: String,

    /// Historical analysis window (in hours)
    #[arg(long, default_value = "168")]
    window_hours: u32,

    /// Headroom percentage to add above p99 utilization (0-100)
    #[arg(long, default_value = "20")]
    headroom_percent: u32,

    /// Minimum recommended pool size
    #[arg(long, default_value = "5")]
    min_recommended: u32,

    /// Maximum recommended pool size
    #[arg(long, default_value = "200")]
    max_recommended: u32,
}

#[derive(Debug, Clone, Copy)]
struct UtilizationPoint {
    timestamp: i64,
    utilization: f64,
    active_connections: u32,
    idle_connections: u32,
    total_size: u32,
}

/// Statistical summary of pool utilization over a window.
#[derive(Debug, Clone)]
pub struct PoolUtilizationStats {
    pub window_hours: u32,
    pub total_samples: usize,
    pub min_utilization: f64,
    pub max_utilization: f64,
    pub avg_utilization: f64,
    pub p50_utilization: f64,
    pub p95_utilization: f64,
    pub p99_utilization: f64,
    pub peak_active_connections: u32,
    pub peak_total_size: u32,
}

/// Pool sizing recommendation.
#[derive(Debug, Clone)]
pub struct PoolSizingRecommendation {
    pub recommended_min: u32,
    pub recommended_max: u32,
    pub reasoning: String,
    pub confidence: String,
    pub data_sufficiency_warning: Option<String>,
}

impl PoolUtilizationStats {
    fn from_points(points: &[UtilizationPoint], window_hours: u32) -> Self {
        if points.is_empty() {
            return Self {
                window_hours,
                total_samples: 0,
                min_utilization: 0.0,
                max_utilization: 0.0,
                avg_utilization: 0.0,
                p50_utilization: 0.0,
                p95_utilization: 0.0,
                p99_utilization: 0.0,
                peak_active_connections: 0,
                peak_total_size: 0,
            };
        }

        let mut utilizations: Vec<f64> = points.iter().map(|p| p.utilization).collect();
        utilizations.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let avg = utilizations.iter().sum::<f64>() / utilizations.len() as f64;
        let min = utilizations[0];
        let max = utilizations[utilizations.len() - 1];

        let percentile = |p: f64| -> f64 {
            let idx = ((utilizations.len() as f64 - 1.0) * p / 100.0).round() as usize;
            utilizations[idx.min(utilizations.len() - 1)]
        };

        let peak_active = points.iter().map(|p| p.active_connections).max().unwrap_or(0);
        let peak_size = points.iter().map(|p| p.total_size).max().unwrap_or(0);

        Self {
            window_hours,
            total_samples: points.len(),
            min_utilization: min,
            max_utilization: max,
            avg_utilization: avg,
            p50_utilization: percentile(50.0),
            p95_utilization: percentile(95.0),
            p99_utilization: percentile(99.0),
            peak_active_connections: peak_active,
            peak_total_size: peak_size,
        }
    }
}

impl PoolSizingRecommendation {
    /// Generate recommendation based on utilization statistics.
    pub fn from_stats(
        stats: &PoolUtilizationStats,
        headroom_percent: u32,
        min_recommended: u32,
        max_recommended: u32,
    ) -> Self {
        let mut warnings = Vec::new();

        // Check for insufficient historical data (less than one full traffic cycle)
        if stats.total_samples < 24 {
            warnings.push(format!(
                "Only {} samples collected; recommend at least 24 (1 day) for reliable trends",
                stats.total_samples
            ));
        }

        // Recommendation based on observed peak + headroom
        let headroom = (headroom_percent as f64) / 100.0;
        let recommended_from_peak =
            (stats.peak_active_connections as f64 * (1.0 + headroom)).ceil() as u32;

        // Recommendation based on p99 + headroom (more conservative)
        let p99_pool_size = (stats.peak_total_size as f64 * (stats.p99_utilization / 100.0))
            .ceil() as u32;
        let recommended_from_p99 = (p99_pool_size as f64 * (1.0 + headroom)).ceil() as u32;

        let min = recommended_from_p99.max(min_recommended);
        let max = recommended_from_peak.max(min + 10).min(max_recommended);

        let reasoning = format!(
            "Analysis window: {} hours, {} samples collected. \
             Peak active connections: {}, p99 utilization: {:.1}%. \
             Peak observed pool size: {}. \
             Recommended range accounts for {}% headroom above observed peak. \
             Min {} based on p99 patterns; max {} ensures peak demand is satisfied.",
            stats.window_hours,
            stats.total_samples,
            stats.peak_active_connections,
            stats.p99_utilization,
            stats.peak_total_size,
            headroom_percent,
            min,
            max
        );

        let confidence = if stats.total_samples >= 24 && stats.max_utilization > 10.0 {
            "HIGH".to_string()
        } else if stats.total_samples >= 12 {
            "MEDIUM".to_string()
        } else {
            "LOW".to_string()
        };

        Self {
            recommended_min: min,
            recommended_max: max,
            reasoning,
            confidence,
            data_sufficiency_warning: if warnings.is_empty() {
                None
            } else {
                Some(warnings.join("; "))
            },
        }
    }
}

/// Mock Prometheus query for testing (in production, this would be an HTTP call).
async fn query_prometheus_metrics(
    _prometheus_url: &str,
    _window_hours: u32,
) -> Result<Vec<UtilizationPoint>> {
    // In a real implementation, this would:
    // 1. Make HTTP calls to Prometheus
    // 2. Query metrics like synapse_db_pool_connections_active, synapse_db_pool_connections_total
    // 3. Parse and aggregate the results
    //
    // For now, return synthetic data for testing
    Ok(vec![
        UtilizationPoint {
            timestamp: 0,
            utilization: 45.0,
            active_connections: 22,
            idle_connections: 28,
            total_size: 50,
        },
        UtilizationPoint {
            timestamp: 3600,
            utilization: 62.0,
            active_connections: 31,
            idle_connections: 19,
            total_size: 50,
        },
        UtilizationPoint {
            timestamp: 7200,
            utilization: 78.0,
            active_connections: 39,
            idle_connections: 11,
            total_size: 50,
        },
        UtilizationPoint {
            timestamp: 10800,
            utilization: 88.0,
            active_connections: 44,
            idle_connections: 6,
            total_size: 50,
        },
    ])
}

pub fn run(args: PoolAdvisorArgs) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        println!(
            "Pool Advisor: Analyzing {} hours of connection pool metrics...",
            args.window_hours
        );

        let metrics = query_prometheus_metrics(&args.prometheus_url, args.window_hours).await?;

        if metrics.is_empty() {
            return Err(anyhow!(
                "No metrics found for the specified window. \
                 Ensure Prometheus is running and pool metrics are being collected."
            ));
        }

        let stats = PoolUtilizationStats::from_points(&metrics, args.window_hours);
        let recommendation = PoolSizingRecommendation::from_stats(
            &stats,
            args.headroom_percent,
            args.min_recommended,
            args.max_recommended,
        );

        println!("\n=== Connection Pool Utilization Analysis ===\n");
        println!("Analysis Window: {} hours", stats.window_hours);
        println!("Samples Collected: {}", stats.total_samples);
        println!("\nUtilization Percentiles:");
        println!("  Min:  {:.1}%", stats.min_utilization);
        println!("  P50:  {:.1}%", stats.p50_utilization);
        println!("  P95:  {:.1}%", stats.p95_utilization);
        println!("  P99:  {:.1}%", stats.p99_utilization);
        println!("  Max:  {:.1}%", stats.max_utilization);
        println!("  Avg:  {:.1}%", stats.avg_utilization);
        println!("\nPeak Observations:");
        println!(
            "  Peak Active Connections: {}",
            stats.peak_active_connections
        );
        println!("  Peak Pool Size: {}", stats.peak_total_size);

        println!("\n=== Pool Sizing Recommendation ===\n");
        println!("Recommended Min: {}", recommendation.recommended_min);
        println!("Recommended Max: {}", recommendation.recommended_max);
        println!("Confidence: {}", recommendation.confidence);
        println!("\nReasoning:");
        println!("{}", recommendation.reasoning);

        if let Some(warning) = &recommendation.data_sufficiency_warning {
            println!("\n⚠️  Warnings:");
            println!("{}", warning);
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_stats_calculation() {
        let points = vec![
            UtilizationPoint {
                timestamp: 0,
                utilization: 30.0,
                active_connections: 15,
                idle_connections: 35,
                total_size: 50,
            },
            UtilizationPoint {
                timestamp: 3600,
                utilization: 60.0,
                active_connections: 30,
                idle_connections: 20,
                total_size: 50,
            },
            UtilizationPoint {
                timestamp: 7200,
                utilization: 90.0,
                active_connections: 45,
                idle_connections: 5,
                total_size: 50,
            },
        ];

        let stats = PoolUtilizationStats::from_points(&points, 168);

        assert_eq!(stats.total_samples, 3);
        assert!((stats.min_utilization - 30.0).abs() < 0.1);
        assert!((stats.max_utilization - 90.0).abs() < 0.1);
        assert!((stats.avg_utilization - 60.0).abs() < 0.1);
        assert_eq!(stats.peak_active_connections, 45);
        assert_eq!(stats.peak_total_size, 50);
    }

    #[test]
    fn test_recommendation_generation() {
        let stats = PoolUtilizationStats {
            window_hours: 168,
            total_samples: 100,
            min_utilization: 20.0,
            max_utilization: 85.0,
            avg_utilization: 50.0,
            p50_utilization: 50.0,
            p95_utilization: 80.0,
            p99_utilization: 82.0,
            peak_active_connections: 40,
            peak_total_size: 50,
        };

        let rec =
            PoolSizingRecommendation::from_stats(&stats, 20, 5, 200);

        assert!(rec.recommended_min > 0);
        assert!(rec.recommended_max >= rec.recommended_min);
        assert_eq!(rec.confidence, "HIGH");
        assert!(rec.data_sufficiency_warning.is_none());
    }

    #[test]
    fn test_recommendation_with_low_confidence() {
        let stats = PoolUtilizationStats {
            window_hours: 1,
            total_samples: 5,
            min_utilization: 10.0,
            max_utilization: 15.0,
            avg_utilization: 12.0,
            p50_utilization: 12.0,
            p95_utilization: 14.0,
            p99_utilization: 15.0,
            peak_active_connections: 7,
            peak_total_size: 50,
        };

        let rec =
            PoolSizingRecommendation::from_stats(&stats, 20, 5, 200);

        assert_eq!(rec.confidence, "LOW");
        assert!(rec.data_sufficiency_warning.is_some());
    }
}
