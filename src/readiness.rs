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
    pub fn new() -> Self {
        Self {
            is_ready: Arc::new(AtomicBool::new(true)),
            drain_timeout_secs: 30,
            is_draining: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a new readiness state with custom drain timeout
    pub fn with_drain_timeout(drain_timeout_secs: u64) -> Self {
        Self {
            is_ready: Arc::new(AtomicBool::new(true)),
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
        assert!(state.is_ready());
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
