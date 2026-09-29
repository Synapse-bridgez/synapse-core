use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, warn, error};

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("Database error: {0}")]
    DatabaseError(String),
    #[error("Coordination sync failed: service is partitioned")]
    PartitionDetected,
    #[error("Invalid state: {0}")]
    InvalidState(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitState {
    pub fn as_str(&self) -> &str {
        match self {
            CircuitState::Closed => "closed",
            CircuitState::Open => "open",
            CircuitState::HalfOpen => "half_open",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, CoordinatorError> {
        match s {
            "closed" => Ok(CircuitState::Closed),
            "open" => Ok(CircuitState::Open),
            "half_open" => Ok(CircuitState::HalfOpen),
            _ => Err(CoordinatorError::InvalidState(format!("Unknown state: {}", s))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerState {
    pub id: String,
    pub service_name: String,
    pub region_name: String,
    pub state: CircuitState,
    pub failure_count: i32,
    pub opened_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub synced: bool,
    pub last_sync_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Coordinates circuit breaker state across multiple regions
pub struct CircuitBreakerCoordinator {
    pool: PgPool,
    service_name: String,
    region_name: String,
    cache: Arc<Mutex<Option<CircuitBreakerState>>>,
    cache_ttl: Duration,
    last_sync: Arc<Mutex<Instant>>,
    sync_failure_threshold: Duration,
}

use std::time::Instant;

impl CircuitBreakerCoordinator {
    pub fn new(
        pool: PgPool,
        service_name: String,
        region_name: String,
        cache_ttl_secs: i64,
        sync_failure_threshold_secs: i64,
    ) -> Self {
        Self {
            pool,
            service_name,
            region_name,
            cache: Arc::new(Mutex::new(None)),
            cache_ttl: Duration::seconds(cache_ttl_secs),
            last_sync: Arc::new(Mutex::new(Instant::now())),
            sync_failure_threshold: Duration::seconds(sync_failure_threshold_secs),
        }
    }

    /// Get the current circuit breaker state, trying to fetch from central store if cache is stale
    pub async fn get_state(&self) -> Result<CircuitBreakerState, CoordinatorError> {
        let last_sync = *self.last_sync.lock().await;
        let elapsed = Instant::now().duration_since(last_sync);

        // Check if cache is stale
        if elapsed.as_secs_f64() > self.cache_ttl.num_seconds() as f64 {
            match self.sync_from_central().await {
                Ok(state) => {
                    *self.cache.lock().await = Some(state.clone());
                    *self.last_sync.lock().await = Instant::now();
                    debug!("Circuit breaker state synced from central store");
                    return Ok(state);
                }
                Err(e) => {
                    // Check if we've been failing to sync for too long (partition detected)
                    if elapsed.as_secs_f64() > self.sync_failure_threshold.num_seconds() as f64 {
                        warn!(
                            "Circuit breaker coordinator has been unable to sync for {} seconds; falling back to local cache",
                            elapsed.as_secs()
                        );
                        if let Some(cached_state) = self.cache.lock().await.as_ref() {
                            return Ok(cached_state.clone());
                        }
                        return Err(CoordinatorError::PartitionDetected);
                    }
                    return Err(e);
                }
            }
        }

        // Return cached state
        if let Some(cached_state) = self.cache.lock().await.as_ref() {
            Ok(cached_state.clone())
        } else {
            // No cache and sync is not stale yet; try to sync
            match self.sync_from_central().await {
                Ok(state) => {
                    *self.cache.lock().await = Some(state.clone());
                    *self.last_sync.lock().await = Instant::now();
                    Ok(state)
                }
                Err(e) => {
                    warn!("Failed to get circuit breaker state: {}", e);
                    Err(e)
                }
            }
        }
    }

    /// Persist a state change to the central store (write-through)
    pub async fn set_state(&self, mut state: CircuitBreakerState) -> Result<(), CoordinatorError> {
        // Write-through: persist to central store first
        self.persist_to_central(&state).await?;

        // On success, update local cache and sync time
        state.synced = true;
        state.last_sync_at = Utc::now();
        *self.cache.lock().await = Some(state);
        *self.last_sync.lock().await = Instant::now();

        debug!("Circuit breaker state persisted and synced");
        Ok(())
    }

    /// Fetch state from central store (PostgreSQL)
    async fn sync_from_central(&self) -> Result<CircuitBreakerState, CoordinatorError> {
        let row = sqlx::query(
            r#"
            SELECT id, service_name, region_name, state, failure_count, opened_at,
                   last_error, last_sync_at, created_at, updated_at
            FROM circuit_breaker_state
            WHERE service_name = $1 AND region_name = $2
            LIMIT 1
            "#,
        )
        .bind(&self.service_name)
        .bind(&self.region_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CoordinatorError::DatabaseError(e.to_string()))?;

        match row {
            Some(r) => {
                let id: String = r.try_get("id").unwrap_or_default();
                let service_name: String = r.try_get("service_name").unwrap_or_default();
                let region_name: String = r.try_get("region_name").unwrap_or_default();
                let state_str: String = r.try_get("state").unwrap_or_else(|_| "closed".to_string());
                let failure_count: i32 = r.try_get("failure_count").unwrap_or(0);
                let opened_at: Option<DateTime<Utc>> = r.try_get("opened_at").ok();
                let last_error: Option<String> = r.try_get("last_error").ok();
                let last_sync_at: DateTime<Utc> = r
                    .try_get("last_sync_at")
                    .unwrap_or_else(|_| Utc::now());
                let created_at: DateTime<Utc> = r
                    .try_get("created_at")
                    .unwrap_or_else(|_| Utc::now());
                let updated_at: DateTime<Utc> = r
                    .try_get("updated_at")
                    .unwrap_or_else(|_| Utc::now());

                Ok(CircuitBreakerState {
                    id,
                    service_name,
                    region_name,
                    state: CircuitState::from_str(&state_str)?,
                    failure_count,
                    opened_at,
                    last_error,
                    synced: true,
                    last_sync_at,
                    created_at,
                    updated_at,
                })
            }
            None => {
                // No entry exists; create default Closed state
                Ok(CircuitBreakerState {
                    id: uuid::Uuid::new_v4().to_string(),
                    service_name: self.service_name.clone(),
                    region_name: self.region_name.clone(),
                    state: CircuitState::Closed,
                    failure_count: 0,
                    opened_at: None,
                    last_error: None,
                    synced: false,
                    last_sync_at: Utc::now(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
            }
        }
    }

    /// Persist state to central store (PostgreSQL) using upsert
    async fn persist_to_central(&self, state: &CircuitBreakerState) -> Result<(), CoordinatorError> {
        sqlx::query(
            r#"
            INSERT INTO circuit_breaker_state
            (id, service_name, region_name, state, failure_count, opened_at, last_error, last_sync_at, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (service_name, region_name) DO UPDATE SET
                state = EXCLUDED.state,
                failure_count = EXCLUDED.failure_count,
                opened_at = EXCLUDED.opened_at,
                last_error = EXCLUDED.last_error,
                last_sync_at = CURRENT_TIMESTAMP,
                updated_at = CURRENT_TIMESTAMP
            "#,
        )
        .bind(&state.id)
        .bind(&state.service_name)
        .bind(&state.region_name)
        .bind(state.state.as_str())
        .bind(state.failure_count)
        .bind(state.opened_at)
        .bind(&state.last_error)
        .bind(Utc::now())
        .bind(state.created_at)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map_err(|e| {
            error!("Failed to persist circuit breaker state: {}", e);
            CoordinatorError::DatabaseError(e.to_string())
        })?;

        Ok(())
    }

    /// Check sync health - returns true if recently synced, false if partitioned
    pub async fn is_healthy(&self) -> bool {
        let last_sync = *self.last_sync.lock().await;
        let elapsed = Instant::now().duration_since(last_sync);
        elapsed.as_secs_f64() < (self.sync_failure_threshold.num_seconds() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_state_conversions() {
        assert_eq!(CircuitState::Closed.as_str(), "closed");
        assert_eq!(CircuitState::Open.as_str(), "open");
        assert_eq!(CircuitState::HalfOpen.as_str(), "half_open");

        assert!(matches!(
            CircuitState::from_str("closed").unwrap(),
            CircuitState::Closed
        ));
        assert!(matches!(
            CircuitState::from_str("open").unwrap(),
            CircuitState::Open
        ));
        assert!(matches!(
            CircuitState::from_str("half_open").unwrap(),
            CircuitState::HalfOpen
        ));

        assert!(CircuitState::from_str("invalid").is_err());
    }

    #[tokio::test]
    async fn test_coordinator_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect("postgres://localhost/test")
            .await
            .ok(); // Allow to fail gracefully in test environments

        if let Some(p) = pool {
            let coordinator = CircuitBreakerCoordinator::new(
                p,
                "test-service".to_string(),
                "us-east-1".to_string(),
                5,   // cache_ttl_secs
                300, // sync_failure_threshold_secs
            );

            // Verify initial state
            assert!(coordinator.is_healthy().await);
        }
    }
}
