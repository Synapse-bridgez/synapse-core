use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Severity level for a dependency
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DependencySeverity {
    /// Critical dependency - if unhealthy, the overall service is unhealthy
    Critical,
    /// Non-critical dependency - if unhealthy, the service is degraded
    NonCritical,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub dependencies: HashMap<String, DependencyStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DependencyStatus {
    Healthy {
        status: String,
        severity: DependencySeverity,
        latency_ms: u64,
    },
    Unhealthy {
        status: String,
        severity: DependencySeverity,
        error: String,
    },
}

/// Health of a single component (for example the database) in the shape every
/// probe endpoint should use: a `status` string and, when the check was timed,
/// a `latency_ms`. Keeping one type here means `/health`, `/ready` and any
/// future probe serialise the same component the same way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

impl ComponentHealth {
    /// A component that answered its check, optionally with how long it took.
    pub fn ok(latency_ms: Option<u64>) -> Self {
        Self {
            status: "ok".to_string(),
            latency_ms,
        }
    }

    /// A component that did not answer its check.
    pub fn down() -> Self {
        Self {
            status: "down".to_string(),
            latency_ms: None,
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == "ok"
    }
}

#[async_trait]
pub trait DependencyChecker: Send + Sync {
    async fn check(&self) -> DependencyStatus;
}

pub struct PostgresChecker {
    pool: sqlx::PgPool,
}

impl PostgresChecker {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DependencyChecker for PostgresChecker {
    async fn check(&self) -> DependencyStatus {
        let start = Instant::now();
        match sqlx::query("SELECT 1").execute(&self.pool).await {
            Ok(_) => DependencyStatus::Healthy {
                status: "healthy".to_string(),
                severity: DependencySeverity::Critical,
                latency_ms: start.elapsed().as_millis() as u64,
            },
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
                severity: DependencySeverity::Critical,
                error: e.to_string(),
            },
        }
    }
}

pub struct RedisChecker {
    url: String,
    circuit_state: Option<String>,
}

impl RedisChecker {
    pub fn new(url: String) -> Self {
        Self {
            url,
            circuit_state: None,
        }
    }

    pub fn with_circuit_state(url: String, circuit_state: String) -> Self {
        Self {
            url,
            circuit_state: Some(circuit_state),
        }
    }
}

#[async_trait]
impl DependencyChecker for RedisChecker {
    async fn check(&self) -> DependencyStatus {
        let start = Instant::now();

        // If circuit is open, report immediately without connecting
        if let Some(ref state) = self.circuit_state {
            if state == "open" {
                return DependencyStatus::Unhealthy {
                    status: "unhealthy".to_string(),
                    severity: DependencySeverity::NonCritical,
                    error: "Redis circuit breaker is open".to_string(),
                };
            }
        }

        match redis::Client::open(self.url.as_str()) {
            Ok(client) => match client.get_multiplexed_async_connection().await {
                Ok(mut conn) => {
                    match redis::cmd("PING").query_async::<_, String>(&mut conn).await {
                        Ok(_) => DependencyStatus::Healthy {
                            status: "healthy".to_string(),
                            severity: DependencySeverity::NonCritical,
                            latency_ms: start.elapsed().as_millis() as u64,
                        },
                        Err(e) => DependencyStatus::Unhealthy {
                            status: "unhealthy".to_string(),
                            severity: DependencySeverity::NonCritical,
                            error: e.to_string(),
                        },
                    }
                }
                Err(e) => DependencyStatus::Unhealthy {
                    status: "unhealthy".to_string(),
                    severity: DependencySeverity::NonCritical,
                    error: e.to_string(),
                },
            },
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
                severity: DependencySeverity::NonCritical,
                error: e.to_string(),
            },
        }
    }
}

pub struct HorizonChecker {
    client: crate::stellar::HorizonClient,
}

impl HorizonChecker {
    pub fn new(client: crate::stellar::HorizonClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DependencyChecker for HorizonChecker {
    async fn check(&self) -> DependencyStatus {
        let start = Instant::now();
        let test_account = "GAAZI4TCR3TY5OJHCTJC2A4QM7S4WXZ3XQFTKJBBHKS3HZXBCXQXQXQX";
        match self.client.get_account(test_account).await {
            Ok(_) | Err(crate::stellar::HorizonError::AccountNotFound(_)) => {
                DependencyStatus::Healthy {
                    status: "healthy".to_string(),
                    severity: DependencySeverity::NonCritical,
                    latency_ms: start.elapsed().as_millis() as u64,
                }
            }
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
                severity: DependencySeverity::NonCritical,
                error: e.to_string(),
            },
        }
    }
}

pub async fn check_health(
    postgres: PostgresChecker,
    redis: RedisChecker,
    horizon: HorizonChecker,
    start_time: Instant,
) -> HealthResponse {
    let timeout_duration = Duration::from_secs(5);

    let (postgres_result, redis_result, horizon_result) = tokio::join!(
        timeout(timeout_duration, postgres.check()),
        timeout(timeout_duration, redis.check()),
        timeout(timeout_duration, horizon.check())
    );

    let mut dependencies = HashMap::new();

    dependencies.insert(
        "postgres".to_string(),
        postgres_result.unwrap_or_else(|_| DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            severity: DependencySeverity::Critical,
            error: "timeout".to_string(),
        }),
    );

    dependencies.insert(
        "redis".to_string(),
        redis_result.unwrap_or_else(|_| DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            severity: DependencySeverity::NonCritical,
            error: "timeout".to_string(),
        }),
    );

    dependencies.insert(
        "horizon".to_string(),
        horizon_result.unwrap_or_else(|_| DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            severity: DependencySeverity::NonCritical,
            error: "timeout".to_string(),
        }),
    );

    let overall_status = determine_overall_status(&dependencies);

    HealthResponse {
        status: overall_status,
        version: "0.1.0".to_string(),
        uptime_seconds: start_time.elapsed().as_secs(),
        dependencies,
    }
}

fn determine_overall_status(dependencies: &HashMap<String, DependencyStatus>) -> String {
    let mut has_critical_failure = false;
    let mut has_non_critical_failure = false;

    for status in dependencies.values() {
        match status {
            DependencyStatus::Unhealthy { severity, .. } => match severity {
                DependencySeverity::Critical => has_critical_failure = true,
                DependencySeverity::NonCritical => has_non_critical_failure = true,
            },
            DependencyStatus::Healthy { .. } => {}
        }
    }

    if has_critical_failure {
        "unhealthy".to_string()
    } else if has_non_critical_failure {
        "degraded".to_string()
    } else {
        "healthy".to_string()
    }
}

#[cfg(test)]
mod component_health_tests {
    use super::*;

    #[test]
    fn ok_with_latency_serialises_both_fields() {
        let json = serde_json::to_value(ComponentHealth::ok(Some(3))).unwrap();
        assert_eq!(json, serde_json::json!({ "status": "ok", "latency_ms": 3 }));
    }

    #[test]
    fn ok_without_latency_omits_the_field() {
        let json = serde_json::to_value(ComponentHealth::ok(None)).unwrap();
        assert_eq!(json, serde_json::json!({ "status": "ok" }));
    }

    #[test]
    fn down_is_not_ok() {
        let health = ComponentHealth::down();
        assert!(!health.is_ok());
        assert_eq!(health.latency_ms, None);
    }
}
