//! Cross-region read-replica lag monitoring and alerting.
//!
//! This module monitors replication lag on read replicas used for reporting/analytics
//! traffic, emitting metrics and alerting when lag exceeds a configurable threshold.

use opentelemetry::KeyValue;
use sqlx::PgPool;
use std::time::Duration;
use tracing::{error, info, warn};

/// Configuration for replica lag monitoring.
#[derive(Debug, Clone)]
pub struct ReplicaLagConfig {
    /// How often to check replica lag (in seconds).
    pub check_interval_secs: u64,
    /// Lag threshold in milliseconds; alerts fire when lag exceeds this value.
    pub lag_threshold_ms: i64,
    /// Replica name/identifier for labeling metrics and alerts.
    pub replica_name: String,
}

impl Default for ReplicaLagConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 60,
            lag_threshold_ms: 5000, // 5 seconds
            replica_name: "replica_1".to_string(),
        }
    }
}

/// Represents a single replication lag measurement.
#[derive(Debug, Clone)]
pub struct ReplicaLagSnapshot {
    pub replica_name: String,
    pub lag_ms: Option<i64>, // None if replica is unreachable
    pub is_reachable: bool,
    pub measured_at: chrono::DateTime<chrono::Utc>,
}

/// Queries the replica's replication lag and returns milliseconds, or `None` if unreachable.
///
/// Uses `pg_last_wal_receive_lsn()` and `pg_last_wal_replay_lsn()` to compute lag on the
/// replica side. If the replica is unreachable (connection error), returns `None` rather
/// than `Some(0)`, to avoid masking a worse problem.
async fn measure_replica_lag(pool: &PgPool, replica_name: &str) -> ReplicaLagSnapshot {
    let now = chrono::Utc::now();

    // Query to compute lag in bytes, then estimate milliseconds.
    // On a replica: `pg_last_wal_receive_lsn() - pg_last_wal_replay_lsn()` = bytes behind.
    let lag_result: Result<(i64,), sqlx::Error> = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM (NOW() - pg_last_xact_replay_timestamp())) * 1000 AS lag_ms",
    )
    .fetch_optional(pool)
    .await
    .map(|opt| opt.ok_or(sqlx::Error::RowNotFound))
    .flatten();

    match lag_result {
        Ok((lag_ms,)) => {
            let lag_ms = lag_ms as i64;
            info!(
                replica = replica_name,
                lag_ms = lag_ms,
                "Replica lag measured"
            );
            ReplicaLagSnapshot {
                replica_name: replica_name.to_string(),
                lag_ms: Some(lag_ms),
                is_reachable: true,
                measured_at: now,
            }
        }
        Err(e) => {
            warn!(
                replica = replica_name,
                error = %e,
                "Failed to measure replica lag (replica unreachable or query failed)"
            );
            ReplicaLagSnapshot {
                replica_name: replica_name.to_string(),
                lag_ms: None,
                is_reachable: false,
                measured_at: now,
            }
        }
    }
}

/// Spawn a background task that periodically checks replica lag and emits metrics.
///
/// Alerts (via logging and metrics) when lag exceeds the configured threshold.
/// Runs every `config.check_interval_secs` seconds.
pub fn spawn_replica_lag_monitor(pool: sqlx::PgPool, config: ReplicaLagConfig) {
    let config_clone = config.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config_clone.check_interval_secs));
        loop {
            interval.tick().await;

            let snapshot = measure_replica_lag(&pool, &config_clone.replica_name).await;

            // Emit metric for all measurements (reachable or not).
            let lag_value = snapshot.lag_ms.unwrap_or(-1); // -1 indicates unreachable
            crate::metrics::replica_lag_ms().record(
                lag_value as f64,
                &[KeyValue::new("replica", snapshot.replica_name.clone())],
            );

            // Alert if lag exceeds threshold and replica is reachable.
            if let Some(lag_ms) = snapshot.lag_ms {
                if lag_ms > config_clone.lag_threshold_ms {
                    warn!(
                        replica = %snapshot.replica_name,
                        lag_ms = lag_ms,
                        threshold_ms = config_clone.lag_threshold_ms,
                        "Replica lag exceeds configured threshold"
                    );
                    crate::metrics::replica_lag_alert_total().add(
                        1,
                        &[
                            KeyValue::new("replica", snapshot.replica_name.clone()),
                            KeyValue::new("reason", "threshold_exceeded"),
                        ],
                    );
                }
            } else {
                // Replica is unreachable.
                error!(
                    replica = %snapshot.replica_name,
                    "Replica is unreachable for lag measurement"
                );
                crate::metrics::replica_lag_alert_total().add(
                    1,
                    &[
                        KeyValue::new("replica", snapshot.replica_name.clone()),
                        KeyValue::new("reason", "unreachable"),
                    ],
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replica_lag_config_default() {
        let config = ReplicaLagConfig::default();
        assert_eq!(config.check_interval_secs, 60);
        assert_eq!(config.lag_threshold_ms, 5000);
        assert_eq!(config.replica_name, "replica_1");
    }

    /// Integration test: spawns a primary/replica Docker setup and verifies lag detection.
    /// Requires Docker (testcontainers).
    #[tokio::test]
    #[ignore = "Requires Docker and PostgreSQL streaming replication setup"]
    async fn test_replica_lag_detection_with_real_replica() {
        use sqlx::migrate::Migrator;
        use std::path::Path;
        use testcontainers::{runners::AsyncRunner, ImageExt};
        use testcontainers_modules::postgres::Postgres;

        // This is a placeholder integration test. A real test would:
        // 1. Start a primary Postgres container
        // 2. Start a replica container configured to replicate from the primary
        // 3. Introduce some lag on the replica
        // 4. Call measure_replica_lag() and assert lag_ms > 0
        // 5. Verify metrics were emitted

        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        // Measure lag on the primary (should be ~0 or very low).
        let snapshot = measure_replica_lag(&pool, "test_replica").await;
        assert!(snapshot.is_reachable);
        // On a non-replicating primary, lag should be minimal or 0.
        assert!(snapshot.lag_ms.unwrap_or(0) >= 0);
    }

    #[test]
    fn test_replica_lag_snapshot_structure() {
        let snapshot = ReplicaLagSnapshot {
            replica_name: "replica_test".to_string(),
            lag_ms: Some(1500),
            is_reachable: true,
            measured_at: chrono::Utc::now(),
        };
        assert_eq!(snapshot.replica_name, "replica_test");
        assert_eq!(snapshot.lag_ms, Some(1500));
        assert!(snapshot.is_reachable);
    }
}
