use chrono::Duration as ChronoDuration;
use std::time::Duration;
use synapse_core::services::CircuitBreaker;

#[tokio::test]
#[ignore = "requires redis"]
async fn test_circuit_breaker_can_be_instantiated() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let redis_client =
        redis::Client::open(redis_url).expect("Failed to create Redis client for testing");

    let breaker = CircuitBreaker::new(
        "test_service".to_string(),
        redis_client,
        5,
        ChronoDuration::seconds(30),
    );

    let state = breaker.get_state().await;
    assert_eq!(state.state, synapse_core::services::CircuitState::Closed);
}

#[tokio::test]
#[ignore = "requires redis"]
async fn test_circuit_breaker_with_config() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let redis_client =
        redis::Client::open(redis_url).expect("Failed to create Redis client for testing");

    let breaker = CircuitBreaker::with_config(
        "test_service".to_string(),
        redis_client,
        5,
        ChronoDuration::seconds(30),
        1,
        Duration::from_millis(100),
        30000,
    );

    let state = breaker.get_state().await;
    assert_eq!(state.state, synapse_core::services::CircuitState::Closed);
}

#[tokio::test]
#[ignore = "requires redis"]
async fn test_circuit_breaker_is_initially_closed() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let redis_client =
        redis::Client::open(redis_url).expect("Failed to create Redis client for testing");

    let breaker = CircuitBreaker::new(
        "test".to_string(),
        redis_client,
        5,
        ChronoDuration::seconds(30),
    );

    let state = breaker.get_state().await;
    assert_eq!(state.state, synapse_core::services::CircuitState::Closed);
}

#[tokio::test]
#[ignore = "requires redis"]
async fn test_circuit_breaker_can_record_failures() {
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://localhost:6379".to_string());

    let redis_client =
        redis::Client::open(redis_url).expect("Failed to create Redis client for testing");

    let breaker = CircuitBreaker::new(
        "test".to_string(),
        redis_client,
        1,
        ChronoDuration::seconds(30),
    );

    let result = breaker.record_failure("Test error").await;
    assert!(result.is_ok(), "Should be able to record failures");

    let state = breaker.get_state().await;
    assert_eq!(
        state.state,
        synapse_core::services::CircuitState::Open,
        "Circuit should open after reaching failure threshold"
    );
}
