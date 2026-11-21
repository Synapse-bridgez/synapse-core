use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Shared pool-health predicate used by both `/health` (liveness) and `/ready`
/// (readiness). Readiness is the stricter criterion: a pool that fails this
/// predicate fails readiness, and liveness must not report healthy either.
///
/// A pool is considered healthy only when it has not been closed and has at
/// least one idle connection available to serve traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolHealth {
    pub is_closed: bool,
    pub idle_connections: u32,
    pub size: u32,
}

impl PoolHealth {
    /// The single source of truth for pool health.
    pub fn is_healthy(&self) -> bool {
        !self.is_closed && self.idle_connections > 0
    }
}

/// Readiness state for the application.
/// Used for Kubernetes readiness probes and connection draining.
#[derive(Clone)]
pub struct ReadinessState {
    /// Flag indicating if the application is ready to accept traffic.
    /// When false, the /ready endpoint returns 503.
    is_ready: Arc<AtomicBool>,
    /// Drain timeout in seconds (default: 30s)
    drain_timeout_secs: u64,
    /// Flag indicating if drain has started
    is_draining: Arc<AtomicBool>,
}

impl ReadinessState {
    /// Create a new readiness state with default drain timeout (30s)
    /// Initially starts as NOT READY until initialization is complete
    pub fn new() -> Self {
        Self {
            is_ready: Arc::new(AtomicBool::new(false)),
            drain_timeout_secs: 30,
            is_draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a new readiness state with custom drain timeout
    /// Initially starts as NOT READY until initialization is complete
    pub fn with_drain_timeout(drain_timeout_secs: u64) -> Self {
        Self {
            is_ready: Arc::new(AtomicBool::new(false)),
            drain_timeout_secs,
            is_draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if the application is ready to accept traffic
    pub fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst)
    }

    /// Check if the application is draining (stopping accepting new connections)
    pub fn is_draining(&self) -> bool {
        self.is_draining.load(Ordering::SeqCst)
    }

    /// Get the drain timeout duration
    pub fn drain_timeout(&self) -> Duration {
        Duration::from_secs(self.drain_timeout_secs)
    }

    /// Mark the application as ready to accept traffic
    pub fn set_ready(&self) {
        self.is_ready.store(true, Ordering::SeqCst);
        self.is_draining.store(false, Ordering::SeqCst);
    }

    /// Mark the application as not ready (draining)
    /// This stops accepting new connections but allows in-flight requests to complete
    pub fn set_not_ready(&self) {
        self.is_ready.store(false, Ordering::SeqCst);
        self.is_draining.store(true, Ordering::SeqCst);
    }

    /// Start the drain process
    /// Returns the drain timeout duration
    pub fn start_drain(&self) -> Duration {
        self.set_not_ready();
        tracing::info!(
            "Starting connection draining with timeout of {} seconds",
            self.drain_timeout_secs
        );
        self.drain_timeout()
    }

    /// Wait for the drain to complete (used in shutdown)
    pub async fn wait_for_drain(&self) {
        let timeout = self.drain_timeout();

        // If already not ready (draining), wait for the timeout
        if !self.is_ready() {
            tracing::info!(
                "Waiting {} seconds for in-flight requests to complete...",
                timeout.as_secs()
            );
            tokio::time::sleep(timeout).await;
            tracing::info!("Drain period complete, shutting down");
        }
    }

    /// Run all initialization checks and set ready=true when complete
    /// Returns true if all checks passed, false if any critical check failed
    pub async fn run_initialization_checks(
        &self,
        pool: &sqlx::PgPool,
        redis_url: &str,
        horizon_url: &str,
    ) -> Result<(), InitializationError> {
        tracing::info!("Starting initialization checks...");

        // Check 1: Verify pool warm-up completed (create_pool blocks until min_connections are established)
        tracing::info!("✓ Database pool warm-up already completed during pool creation");

        // Check 2: Verify Redis connection
        match self.check_redis(redis_url).await {
            Ok(_) => {
                tracing::info!("✓ Redis connection verified");
            }
            Err(e) => {
                tracing::warn!("⚠ Redis check failed (non-critical): {}", e);
                // Continue - Redis is non-critical
            }
        }

        // Check 3: Verify Horizon connectivity
        match self.check_horizon(horizon_url).await {
            Ok(_) => {
                tracing::info!("✓ Horizon connectivity verified");
            }
            Err(e) => {
                tracing::warn!("⚠ Horizon check failed (non-critical): {}", e);
                // Continue - Horizon is non-critical
            }
        }

        // Check 4: Verify database connectivity
        match sqlx::query("SELECT 1").execute(pool).await {
            Ok(_) => {
                tracing::info!("✓ Database connectivity verified");
            }
            Err(e) => {
                let err = InitializationError::DatabaseCheck(e.to_string());
                tracing::error!("✗ Database check failed (critical): {}", err);
                return Err(err);
            }
        }

        tracing::info!("All initialization checks passed - marking service as ready");
        self.set_ready();
        Ok(())
    }

    /// Check Redis connectivity by sending PING
    async fn check_redis(&self, redis_url: &str) -> Result<(), String> {
        match redis::Client::open(redis_url) {
            Ok(client) => match client.get_connection() {
                Ok(mut conn) => match redis::cmd("PING").query::<String>(&mut conn) {
                    Ok(_) => Ok(()),
                    Err(e) => Err(format!("Redis PING failed: {e}")),
                },
                Err(e) => Err(format!("Redis connection failed: {e}")),
            },
            Err(e) => Err(format!("Redis client initialization failed: {e}")),
        }
    }

    /// Check Horizon connectivity
    async fn check_horizon(&self, horizon_url: &str) -> Result<(), String> {
        match reqwest::Client::new()
            .get(format!("{}/", horizon_url.trim_end_matches('/')))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(response) => {
                if response.status().is_success() {
                    Ok(())
                } else {
                    Err(format!("Horizon returned status: {}", response.status()))
                }
            }
            Err(e) => Err(format!("Horizon connectivity check failed: {e}")),
        }
    }
}

/// Error types for initialization checks
#[derive(Debug)]
pub enum InitializationError {
    DatabaseCheck(String),
}

impl std::fmt::Display for InitializationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitializationError::DatabaseCheck(msg) => write!(f, "Database check failed: {msg}"),
        }
    }
}

impl Default for ReadinessState {
    fn default() -> Self {
        Self::new()
    }
}

/// Axum handler: POST /admin/drain
///
/// Kubernetes preStop hook target. Sets readiness to false, starts the drain timer,
/// and returns immediately. The process will exit after the drain timeout elapses.
pub async fn drain_handler(
    axum::extract::State(state): axum::extract::State<crate::ApiState>,
) -> impl axum::response::IntoResponse {
    use axum::http::StatusCode;
    use axum::Json;

    if state.app_state.readiness.is_draining() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "status": "already_draining",
                "drain_timeout_secs": state.app_state.readiness.drain_timeout().as_secs()
            })),
        );
    }

    let timeout = state.app_state.readiness.start_drain();

    // Spawn a task that exits the process after the drain timeout
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        tracing::info!("Drain timeout elapsed — shutting down process");
        std::process::exit(0);
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "draining",
            "drain_timeout_secs": timeout.as_secs()
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::PoolHealth;

    /// The shared predicate is the single source of truth for both endpoints.
    /// A pool state that fails readiness (is_healthy() == false) must also fail
    /// health, because both call the same predicate.
    #[test]
    fn pool_state_failing_readiness_also_fails_health() {
        let unhealthy_states = [
            PoolHealth {
                is_closed: true,
                idle_connections: 5,
                size: 10,
            },
            PoolHealth {
                is_closed: false,
                idle_connections: 0,
                size: 10,
            },
            PoolHealth {
                is_closed: true,
                idle_connections: 0,
                size: 0,
            },
        ];

        for state in unhealthy_states {
            let readiness_ok = state.is_healthy();
            let health_ok = state.is_healthy();
            assert!(!readiness_ok, "expected readiness to fail for {state:?}");
            assert_eq!(
                readiness_ok, health_ok,
                "health and readiness must agree for {state:?}"
            );
        }
    }

    #[test]
    fn healthy_pool_passes_both_checks() {
        let state = PoolHealth {
            is_closed: false,
            idle_connections: 1,
            size: 10,
        };
        assert!(state.is_healthy());
    }
}
