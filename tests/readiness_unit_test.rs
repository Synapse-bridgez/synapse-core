//! Unit tests for the readiness module

use synapse_core::readiness::ReadinessState;

#[test]
fn test_readiness_initial_state() {
    let state = ReadinessState::new();
    assert!(state.is_ready(), "Initial state should be ready");
    assert!(!state.is_draining(), "Initial state should not be draining");
}

#[test]
fn test_set_not_ready() {
    let state = ReadinessState::new();
    state.set_not_ready();
    assert!(
        !state.is_ready(),
        "After set_not_ready, should not be ready"
    );
    assert!(
        state.is_draining(),
        "After set_not_ready, should be draining"
    );
}

#[test]
fn test_set_ready() {
    let state = ReadinessState::new();
    state.set_not_ready();
    state.set_ready();
    assert!(state.is_ready(), "After set_ready, should be ready");
    assert!(
        !state.is_draining(),
        "After set_ready, should not be draining"
    );
}

#[test]
fn test_drain_timeout_custom() {
    let state = ReadinessState::with_drain_timeout(60);
    assert_eq!(
        state.drain_timeout().as_secs(),
        60,
        "Drain timeout should be 60 seconds"
    );
}

#[test]
fn test_drain_timeout_default() {
    let state = ReadinessState::new();
    assert_eq!(
        state.drain_timeout().as_secs(),
        30,
        "Default drain timeout should be 30 seconds"
    );
}

#[test]
fn test_start_drain() {
    let state = ReadinessState::with_drain_timeout(45);
    let timeout = state.start_drain();

    assert_eq!(
        timeout.as_secs(),
        45,
        "start_drain should return configured timeout"
    );
    assert!(!state.is_ready(), "After start_drain, should not be ready");
    assert!(state.is_draining(), "After start_drain, should be draining");
}

#[tokio::test]
async fn test_wait_for_drain() {
    let state = ReadinessState::with_drain_timeout(1);

    // Start drain
    state.start_drain();

    // Wait for drain to complete (should take ~1 second)
    state.wait_for_drain().await;

    // After wait, state should still be not ready (this is expected behavior)
    assert!(
        !state.is_ready(),
        "Should still be not ready after drain wait"
    );
}

#[test]
fn test_readiness_clone() {
    let state = ReadinessState::new();
    let cloned = state.clone();

    // Both should have same initial state
    assert!(cloned.is_ready());

    // Changing original should not affect clone (they share the Arc)
    // Actually they share the same AtomicBool via Arc, so they should be in sync
    state.set_not_ready();
    assert!(
        !cloned.is_ready(),
        "Clone should reflect changes to original (shared Arc)"
    );
}
