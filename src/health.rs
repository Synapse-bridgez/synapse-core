use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::time::timeout;

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
    Healthy { status: String, latency_ms: u64 },
    Unhealthy { status: String, error: String },
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
                latency_ms: start.elapsed().as_millis() as u64,
            },
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
                error: e.to_string(),
            },
        }
    }
}

pub struct RedisChecker {
    url: String,
}

impl RedisChecker {
    pub fn new(url: String) -> Self {
        Self { url }
    }
}

#[async_trait]
impl DependencyChecker for RedisChecker {
    async fn check(&self) -> DependencyStatus {
        let start = Instant::now();
        match redis::Client::open(self.url.as_str()) {
            Ok(client) => match client.get_multiplexed_async_connection().await {
                Ok(mut conn) => {
                    match redis::cmd("PING")
                        .query_async::<_, String>(&mut conn)
                        .await
                    {
                        Ok(_) => DependencyStatus::Healthy {
                            status: "healthy".to_string(),
                            latency_ms: start.elapsed().as_millis() as u64,
                        },
                        Err(e) => DependencyStatus::Unhealthy {
                            status: "unhealthy".to_string(),
                            error: e.to_string(),
                        },
                    }
                }
                Err(e) => DependencyStatus::Unhealthy {
                    status: "unhealthy".to_string(),
                    error: e.to_string(),
                },
            },
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
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
                    latency_ms: start.elapsed().as_millis() as u64,
                }
            }
            Err(e) => DependencyStatus::Unhealthy {
                status: "unhealthy".to_string(),
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
            error: "timeout".to_string(),
        }),
    );

    dependencies.insert(
        "redis".to_string(),
        redis_result.unwrap_or_else(|_| DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
            error: "timeout".to_string(),
        }),
    );

    dependencies.insert(
        "horizon".to_string(),
        horizon_result.unwrap_or_else(|_| DependencyStatus::Unhealthy {
            status: "unhealthy".to_string(),
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
    let critical_deps = ["postgres"];
    let mut has_critical_failure = false;
    let mut has_non_critical_failure = false;

    for (name, status) in dependencies {
        if matches!(status, DependencyStatus::Unhealthy { .. }) {
            if critical_deps.contains(&name.as_str()) {
                has_critical_failure = true;
            } else {
                has_non_critical_failure = true;
            }
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