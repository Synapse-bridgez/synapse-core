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