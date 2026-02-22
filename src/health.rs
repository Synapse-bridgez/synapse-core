use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use utoipa::ToSchema;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime_seconds: u64,
    pub dependencies: HashMap<String, DependencyStatus>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DependencyStatus {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[async_trait]
pub trait DependencyChecker: Send + Sync {
    async fn check(&self) -> DependencyStatus;
    fn name(&self) -> &'static str;
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
            Ok(_) => DependencyStatus {
                status: "healthy".to_string(),
                latency_ms: Some(start.elapsed().as_millis() as u64),
                error: None,
            },
            Err(e) => DependencyStatus {
                status: "unhealthy".to_string(),
                latency_ms: None,
                error: Some(e.to_string()),
            },
        }
    }

    fn name(&self) -> &'static str {
        "postgres"
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
            Ok(client) => {
                match client.get_multiplexed_async_connection().await {
                    Ok(mut conn) => {
                        match redis::cmd("PING").query_async::<_, String>(&mut conn).await {
                            Ok(_) => DependencyStatus {
                                status: "healthy".to_string(),
                                latency_ms: Some(start.elapsed().as_millis() as u64),
                                error: None,
                            },
                            Err(e) => DependencyStatus {
                                status: "unhealthy".to_string(),
                                latency_ms: None,
                                error: Some(e.to_string()),
                            },
                        }
                    }
                    Err(e) => DependencyStatus {
                        status: "unhealthy".to_string(),
                        latency_ms: None,
                        error: Some(e.to_string()),
                    },
                }
            }
            Err(e) => DependencyStatus {
                status: "unhealthy".to_string(),
                latency_ms: None,
                error: Some(e.to_string()),
            },
        }
    }

    fn name(&self) -> &'static str {
        "redis"
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
        // Use a known test account for health check
        match self.client.get_account("GAAZI4TCR3TY5OJHCTJC2A4QSY6CJWJH5IAJTGKIN2ER7LBNVKOCCWN7").await {
            Ok(_) => DependencyStatus {
                status: "healthy".to_string(),
                latency_ms: Some(start.elapsed().as_millis() as u64),
                error: None,
            },
            Err(e) => DependencyStatus {
                status: "unhealthy".to_string(),
                latency_ms: None,
                error: Some(e.to_string()),
            },
        }
    }

    fn name(&self) -> &'static str {
        "horizon"
    }
}

pub struct HealthChecker {
    checkers: Vec<Box<dyn DependencyChecker>>,
    start_time: Instant,
}

impl HealthChecker {
    pub fn new() -> Self {
        Self {
            checkers: Vec::new(),
            start_time: Instant::now(),
        }
    }

    pub fn add_checker(mut self, checker: Box<dyn DependencyChecker>) -> Self {
        self.checkers.push(checker);
        self
    }

    pub async fn check_all(&self) -> HealthResponse {
        let check_timeout = Duration::from_secs(5);
        let mut futures = Vec::new();

        for checker in &self.checkers {
            let name = checker.name().to_string();
            let future = timeout(check_timeout, checker.check());
            futures.push(async move {
                match future.await {
                    Ok(status) => (name, status),
                    Err(_) => (
                        name,
                        DependencyStatus {
                            status: "unhealthy".to_string(),
                            latency_ms: None,
                            error: Some("timeout".to_string()),
                        },
                    ),
                }
            });
        }

        let results = futures::future::join_all(futures).await;
        let mut dependencies = HashMap::new();
        let mut healthy_count = 0;
        let mut total_count = 0;

        for (name, status) in results {
            if status.status == "healthy" {
                healthy_count += 1;
            }
            total_count += 1;
            dependencies.insert(name, status);
        }

        let overall_status = if healthy_count == total_count {
            "healthy"
        } else if healthy_count > 0 {
            "degraded"
        } else {
            "unhealthy"
        };

        HealthResponse {
            status: overall_status.to_string(),
            version: "0.1.0".to_string(),
            uptime_seconds: self.start_time.elapsed().as_secs(),
            dependencies,
        }
    }
}