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

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphHealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct HealthGraphObservation {
    pub id: String,
    pub status: GraphHealthStatus,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct HealthGraphNode {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub status: GraphHealthStatus,
    pub own_status: GraphHealthStatus,
    pub dependency_status: GraphHealthStatus,
    pub dependencies: Vec<String>,
    pub affected_by: Vec<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct HealthGraphEdge {
    pub source: String,
    pub target: String,
    pub critical: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct HealthGraphResponse {
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub status: GraphHealthStatus,
    pub nodes: Vec<HealthGraphNode>,
    pub edges: Vec<HealthGraphEdge>,
}

const HEALTH_GRAPH_EDGES: &[(&str, &str, bool)] = &[
    ("service", "postgres", true),
    ("service", "redis", false),
    ("service", "vault", false),
    ("service", "settlement_network", false),
];

/// Combine live dependency observations with the declared service topology.
///
/// `own_status` describes the service itself (readiness/draining); dependency
/// failures are kept in separate fields so a healthy process is not confused
/// with the upstream failure that is degrading it.
pub fn build_health_graph(
    own_status: GraphHealthStatus,
    own_detail: Option<String>,
    observations: impl IntoIterator<Item = HealthGraphObservation>,
) -> HealthGraphResponse {
    let observations: HashMap<String, HealthGraphObservation> = observations
        .into_iter()
        .map(|observation| (observation.id.clone(), observation))
        .collect();

    let edges: Vec<HealthGraphEdge> = HEALTH_GRAPH_EDGES
        .iter()
        .map(|(source, target, critical)| HealthGraphEdge {
            source: (*source).to_string(),
            target: (*target).to_string(),
            critical: *critical,
        })
        .collect();
    let dependency_ids: Vec<String> = HEALTH_GRAPH_EDGES
        .iter()
        .map(|(_, target, _)| (*target).to_string())
        .collect();

    let affected_by: Vec<String> = dependency_ids
        .iter()
        .filter(|id| {
            observations
                .get(*id)
                .is_some_and(|observation| observation.status != GraphHealthStatus::Healthy
                    && observation.status != GraphHealthStatus::Unknown)
        })
        .cloned()
        .collect();
    let dependency_statuses: Vec<GraphHealthStatus> = dependency_ids
        .iter()
        .map(|id| {
            observations
                .get(id)
                .map_or(GraphHealthStatus::Unknown, |observation| observation.status)
        })
        .collect();
    let dependency_status = if dependency_statuses.contains(&GraphHealthStatus::Unhealthy) {
        GraphHealthStatus::Unhealthy
    } else if dependency_statuses.contains(&GraphHealthStatus::Degraded) {
        GraphHealthStatus::Degraded
    } else if dependency_statuses.contains(&GraphHealthStatus::Unknown) {
        GraphHealthStatus::Unknown
    } else {
        GraphHealthStatus::Healthy
    };
    let critical_dependency_failed = HEALTH_GRAPH_EDGES
        .iter()
        .filter(|(_, _, critical)| *critical)
        .any(|(_, target, _)| {
            observations
                .get(*target)
                .is_some_and(|observation| observation.status == GraphHealthStatus::Unhealthy)
        });
    let status = if own_status == GraphHealthStatus::Unhealthy || critical_dependency_failed {
        GraphHealthStatus::Unhealthy
    } else if own_status == GraphHealthStatus::Degraded
        || dependency_status == GraphHealthStatus::Degraded
        || dependency_status == GraphHealthStatus::Unhealthy
    {
        GraphHealthStatus::Degraded
    } else if own_status == GraphHealthStatus::Unknown
        || dependency_status == GraphHealthStatus::Unknown
    {
        GraphHealthStatus::Unknown
    } else {
        GraphHealthStatus::Healthy
    };

    let mut nodes = vec![HealthGraphNode {
        id: "service".to_string(),
        name: "Synapse Core".to_string(),
        kind: "service".to_string(),
        status,
        own_status,
        dependency_status,
        dependencies: dependency_ids,
        affected_by,
        detail: own_detail,
    }];
    for (id, name) in [
        ("postgres", "Postgres"),
        ("redis", "Redis"),
        ("vault", "Vault"),
        ("settlement_network", "Settlement network API"),
    ] {
        let observation = observations.get(id);
        let node_status = observation
            .map_or(GraphHealthStatus::Unknown, |observation| observation.status);
        nodes.push(HealthGraphNode {
            id: id.to_string(),
            name: name.to_string(),
            kind: "dependency".to_string(),
            status: node_status,
            own_status: node_status,
            dependency_status: GraphHealthStatus::Healthy,
            dependencies: Vec::new(),
            affected_by: Vec::new(),
            detail: observation.and_then(|observation| observation.detail.clone()),
        });
    }

    HealthGraphResponse {
        generated_at: chrono::Utc::now(),
        status,
        nodes,
        edges,
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
mod graph_tests {
    use super::*;

    fn observations(
        overrides: &[(&str, GraphHealthStatus)],
    ) -> Vec<HealthGraphObservation> {
        ["postgres", "redis", "vault", "settlement_network"]
            .into_iter()
            .map(|id| HealthGraphObservation {
                id: id.to_string(),
                status: overrides
                    .iter()
                    .find(|(override_id, _)| *override_id == id)
                    .map_or(GraphHealthStatus::Healthy, |(_, status)| *status),
                detail: None,
            })
            .collect()
    }

    #[test]
    fn healthy_dependencies_produce_expected_edges() {
        let graph = build_health_graph(
            GraphHealthStatus::Healthy,
            None,
            observations(&[]),
        );

        assert_eq!(graph.status, GraphHealthStatus::Healthy);
        assert_eq!(graph.nodes.len(), 5);
        assert_eq!(graph.edges.len(), 4);
        assert_eq!(graph.nodes[0].dependencies.len(), 4);
        assert!(graph.edges.iter().any(|edge| {
            edge.source == "service" && edge.target == "postgres" && edge.critical
        }));
        assert!(graph.edges.iter().any(|edge| {
            edge.source == "service" && edge.target == "settlement_network" && !edge.critical
        }));
        let json = serde_json::to_value(&graph).unwrap();
        assert_eq!(json["status"], "healthy");
        assert_eq!(json["nodes"][0]["own_status"], "healthy");
        assert_eq!(json["nodes"][0]["dependency_status"], "healthy");
        assert_eq!(json["edges"][0]["source"], "service");
        assert_eq!(json["edges"][0]["target"], "postgres");
    }

    #[test]
    fn dependency_failure_is_distinct_from_service_health() {
        let graph = build_health_graph(
            GraphHealthStatus::Healthy,
            None,
            observations(&[("redis", GraphHealthStatus::Unhealthy)]),
        );
        let service = &graph.nodes[0];

        assert_eq!(graph.status, GraphHealthStatus::Degraded);
        assert_eq!(service.own_status, GraphHealthStatus::Healthy);
        assert_eq!(service.dependency_status, GraphHealthStatus::Unhealthy);
        assert_eq!(service.affected_by, vec!["redis".to_string()]);
        assert_eq!(graph.nodes[2].status, GraphHealthStatus::Unhealthy);
    }

    #[test]
    fn critical_postgres_failure_makes_service_unhealthy() {
        let graph = build_health_graph(
            GraphHealthStatus::Healthy,
            None,
            observations(&[("postgres", GraphHealthStatus::Unhealthy)]),
        );

        assert_eq!(graph.status, GraphHealthStatus::Unhealthy);
        assert_eq!(graph.nodes[0].own_status, GraphHealthStatus::Healthy);
        assert_eq!(graph.nodes[0].dependency_status, GraphHealthStatus::Unhealthy);
        assert_eq!(graph.nodes[0].affected_by, vec!["postgres".to_string()]);
    }

    #[test]
    fn missing_dependency_observation_is_reported_unknown() {
        let mut observations = observations(&[]);
        observations.retain(|observation| observation.id != "vault");
        let graph = build_health_graph(
            GraphHealthStatus::Healthy,
            None,
            observations,
        );

        assert_eq!(graph.status, GraphHealthStatus::Unknown);
        assert_eq!(graph.nodes[0].own_status, GraphHealthStatus::Healthy);
        assert_eq!(graph.nodes[0].dependency_status, GraphHealthStatus::Unknown);
        assert_eq!(graph.nodes[3].status, GraphHealthStatus::Unknown);
    }

    #[test]
    fn degraded_dependency_degrades_service_without_marking_it_unhealthy() {
        let graph = build_health_graph(
            GraphHealthStatus::Healthy,
            None,
            observations(&[("vault", GraphHealthStatus::Degraded)]),
        );

        assert_eq!(graph.status, GraphHealthStatus::Degraded);
        assert_eq!(graph.nodes[0].own_status, GraphHealthStatus::Healthy);
        assert_eq!(graph.nodes[0].dependency_status, GraphHealthStatus::Degraded);
        assert_eq!(graph.nodes[0].affected_by, vec!["vault".to_string()]);
    }

    #[test]
    fn service_degradation_is_not_attributed_to_dependencies() {
        let graph = build_health_graph(
            GraphHealthStatus::Degraded,
            Some("Service is draining".to_string()),
            observations(&[]),
        );

        assert_eq!(graph.status, GraphHealthStatus::Degraded);
        assert_eq!(graph.nodes[0].own_status, GraphHealthStatus::Degraded);
        assert_eq!(graph.nodes[0].dependency_status, GraphHealthStatus::Healthy);
        assert!(graph.nodes[0].affected_by.is_empty());
    }

    #[test]
    fn unhealthy_service_is_distinct_from_healthy_dependencies() {
        let graph = build_health_graph(
            GraphHealthStatus::Unhealthy,
            Some("Service initialization failed".to_string()),
            observations(&[]),
        );

        assert_eq!(graph.status, GraphHealthStatus::Unhealthy);
        assert_eq!(graph.nodes[0].own_status, GraphHealthStatus::Unhealthy);
        assert_eq!(graph.nodes[0].dependency_status, GraphHealthStatus::Healthy);
        assert!(graph.nodes[0].affected_by.is_empty());
    }
}
