use std::sync::Arc;
use tokio::sync::Mutex;
use redis::Client as RedisClient;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc, Duration};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CircuitBreakerError {
    #[error("Circuit breaker is open")]
    Open,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerState {
    pub state: CircuitState,
    pub opened_at: Option<DateTime<Utc>>,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct CircuitBreaker {
    service_name: String,
    redis_client: RedisClient,
    state: Arc<Mutex<CircuitBreakerState>>,
    failure_threshold: u32,
    reset_timeout: Duration,
}

impl CircuitBreaker {
    pub fn new(
        service_name: String,
        redis_client: RedisClient,
        failure_threshold: u32,
        reset_timeout: Duration,
    ) -> Self {
        let state = CircuitBreakerState {
            state: CircuitState::Closed,
            opened_at: None,
            failure_count: 0,
            last_error: None,
        };
        Self {
            service_name,
            redis_client,
            state: Arc::new(Mutex::new(state)),
            failure_threshold,
            reset_timeout,
        }
    }

    pub async fn load_from_redis(&self) -> Result<(), redis::RedisError> {
        let key = format!("cb:state:{}", self.service_name);
        let mut conn = self.redis_client.get_async_connection().await?;
        let data: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut conn).await?;
        if let Some(json) = data {
            let persisted_state: CircuitBreakerState = serde_json::from_str(&json)?;
            *self.state.lock().await = persisted_state;
        }
        Ok(())
    }

    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let mut state = self.state.lock().await;
        match state.state {
            CircuitState::Open => {
                if Utc::now().signed_duration_since(state.opened_at.unwrap()) > self.reset_timeout {
                    state.state = CircuitState::HalfOpen;
                } else {
                    return Err(Box::new(CircuitBreakerError::Open));
                }
            }
            _ => {}
        }
        drop(state); // unlock

        let result = f().await;

        let mut state = self.state.lock().await;
        match &result {
            Ok(_) => {
                state.failure_count = 0;
                state.state = CircuitState::Closed;
                state.opened_at = None;
                state.last_error = None;
            }
            Err(e) => {
                state.failure_count += 1;
                state.last_error = Some(e.to_string());
                if state.failure_count >= self.failure_threshold {
                    state.state = CircuitState::Open;
                    state.opened_at = Some(Utc::now());
                    // Persist
                    if let Err(persist_err) = self.persist_to_redis(&state).await {
                        tracing::error!("Failed to persist circuit breaker state: {}", persist_err);
                    }
                }
            }
        }
        result
    }

    async fn persist_to_redis(&self, state: &CircuitBreakerState) -> Result<(), redis::RedisError> {
        let key = format!("cb:state:{}", self.service_name);
        let json = serde_json::to_string(state)?;
        let mut conn = self.redis_client.get_async_connection().await?;
        redis::cmd("SETEX")
            .arg(&key)
            .arg(self.reset_timeout.num_seconds())
            .arg(json)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    pub async fn get_state(&self) -> CircuitBreakerState {
        self.state.lock().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a CircuitBreaker with an unreachable Redis URL.
    /// The client is lazy – construction succeeds; only actual I/O would fail.
    fn make_cb(threshold: u32, reset_secs: i64) -> CircuitBreaker {
        let client = RedisClient::open("redis://127.0.0.1:1/").unwrap();
        CircuitBreaker::new(
            "test-service".to_string(),
            client,
            threshold,
            Duration::seconds(reset_secs),
        )
    }

    fn fail() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("simulated failure".into())
    }

    fn ok() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    // ── Closed → Open ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn closed_transitions_to_open_after_threshold() {
        let cb = make_cb(3, 60);

        // Two failures – still Closed
        for _ in 0..2 {
            let _ = cb.call(|| async { fail() }).await;
        }
        assert!(matches!(cb.get_state().await.state, CircuitState::Closed));

        // Third failure crosses threshold → Open
        let _ = cb.call(|| async { fail() }).await;
        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
        assert_eq!(state.failure_count, 3);
    }

    // ── Open → HalfOpen ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_transitions_to_half_open_after_reset_timeout() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s → expires immediately

        // Trip the breaker
        let _ = cb.call(|| async { fail() }).await;
        assert!(matches!(cb.get_state().await.state, CircuitState::Open));

        // Next call: timeout has elapsed → breaker moves to HalfOpen and the
        // inner function executes.  We return an error so it trips back to Open,
        // but the important thing is that the call was *attempted* (not fast-failed).
        let result = cb.call(|| async { fail() }).await;
        // The call was forwarded (not short-circuited with CircuitBreakerError::Open)
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert_ne!(err_msg, "Circuit breaker is open");
    }

    // ── HalfOpen → Closed ────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_closed_on_success() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Probe succeeds → Closed
        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_ok());

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Closed));
        assert_eq!(state.failure_count, 0);
        assert!(state.opened_at.is_none());
    }

    // ── HalfOpen → Open ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_open_on_failure() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Probe fails → back to Open
        let _ = cb.call(|| async { fail() }).await;

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
    }

    // ── Open fast-fails while timeout has not elapsed ─────────────────────────

    #[tokio::test]
    async fn open_fast_fails_before_reset_timeout() {
        let cb = make_cb(1, 3600); // reset_timeout = 1 hour

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Immediate call → fast-fail
        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Circuit breaker is open");
    }
}