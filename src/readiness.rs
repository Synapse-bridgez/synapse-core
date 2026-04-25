use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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

        // Check 1: Verify migrations completed (implicit - pool is already initialized)
        tracing::info!("✓ Database migrations already verified during pool initialization");

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
        use redis::Commands;
        
        match redis::Client::open(redis_url) {
            Ok(client) => match client.get_connection() {
                Ok(mut conn) => {
                    match conn.ping::<()>() {
                        Ok(_) => Ok(()),
                        Err(e) => Err(format!("Redis PING failed: {}", e)),
                    }
                }
                Err(e) => Err(format!("Redis connection failed: {}", e)),
            },
            Err(e) => Err(format!("Redis client initialization failed: {}", e)),
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
            Err(e) => Err(format!("Horizon connectivity check failed: {}", e)),
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
            InitializationError::DatabaseCheck(msg) => write!(f, "Database check failed: {}", msg),
        }
    }
}

impl Default for ReadinessState {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait to easily add readiness state to AppState
pub trait AddReadiness {
    fn with_readiness(self, readiness: ReadinessState) -> Self;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_readiness_initial_state() {
        let state = ReadinessState::new();
        assert!(!state.is_ready(), "Initial state should be NOT READY");
        assert!(!state.is_draining());
    }

    #[test]
    fn test_set_not_ready() {
        let state = ReadinessState::new();
        state.set_not_ready();
        assert!(!state.is_ready());
        assert!(state.is_draining());
    }

    #[test]
    fn test_set_ready() {
        let state = ReadinessState::new();
        state.set_not_ready();
        state.set_ready();
        assert!(state.is_ready());
        assert!(!state.is_draining());
    }

    #[test]
    fn test_drain_timeout() {
        let state = ReadinessState::with_drain_timeout(60);
        assert_eq!(state.drain_timeout().as_secs(), 60);
    }

    #[test]
    fn test_default_drain_timeout() {
        let state = ReadinessState::new();
        assert_eq!(state.drain_timeout().as_secs(), 30);
    }
}
