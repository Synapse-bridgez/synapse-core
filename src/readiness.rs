use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        let started_at = std::time::Instant::now();

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
                tracing::error!(
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "✗ Database check failed (critical): {}",
                    err
                );
                crate::metrics::readiness_initialization_duration_ms().record(
                    started_at.elapsed().as_secs_f64() * 1000.0,
                    &[opentelemetry::KeyValue::new("outcome", "failed")],
                );
                return Err(err);
            }
        }

        let elapsed_ms = started_at.elapsed().as_millis() as u64;
        tracing::info!(
            elapsed_ms,
            "All initialization checks passed - marking service as ready"
        );
        crate::metrics::readiness_initialization_duration_ms().record(
            elapsed_ms as f64,
            &[opentelemetry::KeyValue::new("outcome", "ready")],
        );
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

// ---------------------------------------------------------------------------
// Dependency degradation on /ready (#1335, #1336)
// ---------------------------------------------------------------------------

/// Vault state as reported on `/ready`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct VaultReadiness {
    /// `not_configured` | `ok` | `degraded_cached_fallback` | `expired`
    pub status: String,
    /// How long Vault has been failing, if it is.
    pub unreachable_for_secs: Option<u64>,
    /// Time left before cached secrets hit their hard maximum age and start
    /// being refused.
    pub fallback_remaining_secs: Option<u64>,
    pub max_fallback_age_secs: Option<u64>,
}

/// Redis degraded-mode blast radius as reported on `/ready`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RedisReadiness {
    /// `ok` | `degraded`
    pub status: String,
    /// Components that degraded around a Redis failure within the last
    /// minute (see `cache::degradation`).
    pub degraded_components: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DependencyReadiness {
    pub vault: VaultReadiness,
    pub redis: RedisReadiness,
}

impl DependencyReadiness {
    /// Names of degraded dependencies, for the `X-Degraded-Dependencies`
    /// header and the `degraded` body field.
    pub fn degraded(&self) -> Vec<String> {
        let mut out = Vec::new();
        if matches!(
            self.vault.status.as_str(),
            "degraded_cached_fallback" | "expired"
        ) {
            out.push("vault".to_string());
        }
        if self.redis.status == "degraded" {
            out.push("redis".to_string());
        }
        out
    }

    /// True once cached Vault secrets have passed their hard maximum age.
    pub fn secrets_expired(&self) -> bool {
        self.vault.status == "expired"
    }
}

/// Builds the dependency section of `/ready`.
///
/// Degradation is *reported*, not turned into a 503: a transient Vault or
/// Redis blip is exactly what the fallbacks exist to ride out, and failing
/// readiness on every instance at once would turn it into a full outage.
/// Orchestration-level alerting keys off the header / body (and the
/// `vault_fallback_active` / `redis_degraded_operations_total` metrics)
/// instead. Set `READINESS_FAIL_ON_EXPIRED_SECRETS=true` to have `/ready`
/// return 503 once cached secrets have expired.
pub fn dependency_readiness(
    secrets: Option<&crate::secrets::SecretsStore>,
    now: Instant,
) -> DependencyReadiness {
    let vault = match secrets {
        None => VaultReadiness {
            status: "not_configured".to_string(),
            unreachable_for_secs: None,
            fallback_remaining_secs: None,
            max_fallback_age_secs: None,
        },
        Some(store) => {
            let v = store.vault_status_at(now);
            VaultReadiness {
                status: v.status.to_string(),
                unreachable_for_secs: v.unreachable_for_secs,
                fallback_remaining_secs: v.fallback_remaining_secs,
                max_fallback_age_secs: Some(v.max_fallback_age_secs),
            }
        }
    };
    let degraded_components: Vec<String> = crate::cache::degradation::active_components()
        .into_iter()
        .map(|c| c.as_str().to_string())
        .collect();
    let redis = RedisReadiness {
        status: if degraded_components.is_empty() {
            "ok"
        } else {
            "degraded"
        }
        .to_string(),
        degraded_components,
    };
    DependencyReadiness { vault, redis }
}

/// Whether `/ready` should fail once cached secrets have expired
/// (`READINESS_FAIL_ON_EXPIRED_SECRETS`, default false).
pub fn fail_on_expired_secrets() -> bool {
    std::env::var("READINESS_FAIL_ON_EXPIRED_SECRETS")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
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

    let connections_open_at_start = state.app_state.ws_connection_pool.active_connections();
    crate::metrics::ws_drain_connections_open_at_start()
        .record(connections_open_at_start as f64, &[]);

    // Spawn a task that exits the process after the drain timeout
    let drain_start = Instant::now();
    let ws_pool = state.app_state.ws_connection_pool.clone();
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;

        // Connections still open at the deadline had to be forcibly
        // terminated by process exit rather than closing themselves in
        // response to the drain signal (see `handle_socket`'s drain check).
        let (clean, forced) =
            drain_close_outcome(connections_open_at_start, ws_pool.active_connections());
        let closed_total = crate::metrics::ws_drain_connections_closed_total();
        if clean > 0 {
            closed_total.add(
                clean as u64,
                &[opentelemetry::KeyValue::new("outcome", "clean")],
            );
        }
        if forced > 0 {
            closed_total.add(
                forced as u64,
                &[opentelemetry::KeyValue::new("outcome", "forced")],
            );
            tracing::warn!(
                forced_close_count = forced,
                "Drain timeout elapsed with connections still open — forcibly closing"
            );
        }
        crate::metrics::ws_drain_duration_ms()
            .record(drain_start.elapsed().as_millis() as f64, &[]);

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

/// Splits the WebSocket connections open at drain start into those closed
/// cleanly (in response to the drain signal, before the deadline) vs those
/// still open at the deadline and therefore forcibly terminated by process
/// exit. Returns `(clean, forced)`.
fn drain_close_outcome(open_at_start: usize, remaining_at_deadline: usize) -> (usize, usize) {
    let forced = remaining_at_deadline.min(open_at_start);
    let clean = open_at_start - forced;
    (clean, forced)
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

    /// Orchestrator-compatibility requirement: readiness must flip to
    /// not-ready synchronously at the start of the drain, before any
    /// in-flight-request wait — otherwise the orchestrator could keep
    /// routing new traffic for the duration of the drain timeout.
    #[test]
    fn test_shutdown_drain_flips_readiness_immediately() {
        let state = ReadinessState::new();
        state.set_ready();
        assert!(state.is_ready());

        state.start_drain();

        assert!(
            !state.is_ready(),
            "readiness must flip to not-ready as soon as drain starts, not after the timeout"
        );
        assert!(state.is_draining());
    }

    #[test]
    fn readiness_without_vault_reports_not_configured() {
        let r = dependency_readiness(None, Instant::now());
        assert_eq!(r.vault.status, "not_configured");
        assert!(!r.degraded().contains(&"vault".to_string()));
        assert!(!r.secrets_expired());
    }

    #[test]
    fn readiness_surfaces_vault_fallback_before_hard_limit() {
        use crate::secrets::{SecretKind, SecretsStore, VaultFallbackConfig};
        let store = SecretsStore::with_fallback_config(
            "a".into(),
            "b".into(),
            VaultFallbackConfig::clamped(Some(Duration::from_secs(900)), None),
        );
        let t0 = Instant::now();
        for kind in SecretKind::ALL {
            store.record_refresh_success(kind, t0);
            store.record_refresh_failure(kind, t0, &"down");
        }
        let during = dependency_readiness(Some(&store), t0 + Duration::from_secs(120));
        assert_eq!(during.vault.status, "degraded_cached_fallback");
        assert_eq!(during.vault.fallback_remaining_secs, Some(780));
        assert_eq!(during.vault.unreachable_for_secs, Some(120));
        assert!(during.degraded().contains(&"vault".to_string()));
        assert!(!during.secrets_expired());

        let after = dependency_readiness(Some(&store), t0 + Duration::from_secs(1000));
        assert_eq!(after.vault.status, "expired");
        assert!(after.secrets_expired());
    }

    #[test]
    fn readiness_lists_redis_degraded_components() {
        crate::cache::degradation::record_redis_degraded(
            crate::cache::degradation::RedisComponent::QueryCache,
            crate::cache::degradation::DegradedFallback::DirectDbRead,
            &"down",
        );
        let r = dependency_readiness(None, Instant::now());
        assert_eq!(r.redis.status, "degraded");
        assert!(r
            .redis
            .degraded_components
            .contains(&"query_cache".to_string()));
        assert!(r.degraded().contains(&"redis".to_string()));
    }

    #[test]
    fn fail_on_expired_defaults_off() {
        if std::env::var("READINESS_FAIL_ON_EXPIRED_SECRETS").is_err() {
            assert!(!fail_on_expired_secrets());
        }
    }

    /// A migration/dependency check that is slow-but-progressing must keep
    /// readiness false throughout (never flip early) and only flip once the
    /// check actually completes.
    #[tokio::test]
    async fn test_slow_dependency_startup_stays_not_ready_until_complete() {
        let state = ReadinessState::new();
        assert!(!state.is_ready());

        let simulated_check = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        simulated_check.await;
        assert!(
            !state.is_ready(),
            "must remain not-ready while a slow startup dependency check is in progress"
        );

        state.set_ready();
        assert!(state.is_ready());
    }
}
