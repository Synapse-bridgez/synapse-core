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

/// Reports Vault reachability for the readiness endpoint.
///
/// When Vault is unreachable at runtime, the secret cache may still serve
/// last-known-good non-database secrets within its bounded TTL window. This
/// checker surfaces that state so orchestration-level alerting can catch a
/// Vault outage even before the cache's hard maximum age is reached.
pub struct VaultChecker {
    client: Option<vaultrs::client::VaultClient>,
    cache_age: Option<Duration>,
    cache_max_age: Duration,
}

impl VaultChecker {
    pub fn new(client: vaultrs::client::VaultClient) -> Self {
        Self {
            client: Some(client),
            cache_age: None,
            cache_max_age: Duration::from_secs(60),
        }
    }

    /// Build a checker that reports the current cached-secret fallback state
    /// without performing a live Vault probe.
    pub fn from_cache_state(cache_age: Option<Duration>, cache_max_age: Duration) -> Self {
        Self {
            client: None,
            cache_age,
            cache_max_age,
        }
    }
}

#[async_trait]
impl DependencyChecker for VaultChecker {
    async fn check(&self) -> DependencyStatus {
        let start = Instant::now();

        if let Some(ref client) = self.client {
            match vaultrs::sys::health(client).await {
                Ok(_) => {
                    return DependencyStatus::Healthy {
                        status: "healthy".to_string(),
                        severity: DependencySeverity::NonCritical,
                        latency_ms: start.elapsed().as_millis() as u64,
                    };
                }
                Err(e) => {
                    return self.degraded_status(e.to_string());
                }
            }
        }

        self.degraded_status("Vault unreachable; serving cached secrets".to_string())
    }
}

impl VaultChecker {
    fn degraded_status(&self, error: String) -> DependencyStatus {
        match self.cache_age {
            Some(age) if age <= self.cache_max_age => DependencyStatus::Unhealthy {
                status: "degraded".to_string(),
                severity: DependencySeverity::NonCritical,
                error: format!(
                    "{} (cached secrets in use, age {}s of max {}s)",
                    error,
                    age.as_secs(),
                    self.cache_max_age.as_secs()
                ),
            },
            _ => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
                severity: DependencySeverity::Critical,
                error: format!(
                    "{} (cached secrets expired past max age {}s)",
                    error,
                    self.cache_max_age.as_secs()
                ),
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
