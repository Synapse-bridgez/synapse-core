# Circuit Breaker for Stellar Horizon (Resilience)

## Overview
This PR implements a circuit breaker pattern for the Stellar Horizon client to protect the system from cascading failures when the Horizon API is down or experiencing issues.

## Changes

### Core Implementation
- **Added circuit breaker to `HorizonClient`** using the `failsafe` crate
- **Three states**: Closed (normal) → Open (fail fast) → Half-Open (probing)
- **Configurable thresholds**: Default 3 consecutive failures, 60-120s reset timeout
- **New error variant**: `HorizonError::CircuitBreakerOpen` for rejected requests

### API Changes
- `HorizonClient::new()` - Creates client with default circuit breaker
- `HorizonClient::with_circuit_breaker()` - Creates client with custom configuration
- `HorizonClient::circuit_state()` - Returns current breaker state

### Dependencies
- Added `failsafe` v1.3 for circuit breaker logic
- Added `ipnet` v2 (required by config module)

### Tests
- Unit tests for circuit breaker state management
- Integration test demonstrating circuit opening after failures
- All existing tests pass

### Documentation
- Comprehensive guide in `docs/circuit-breaker.md`
- Updated `README.md` with circuit breaker section
- Inline code documentation

## Benefits
1. **Prevents resource exhaustion** - Worker threads don't pile up waiting for timeouts
2. **Fast failure** - Immediate rejection when service is known to be down
3. **Automatic recovery** - Probes for service recovery after timeout
4. **Configurable** - Adjust thresholds based on requirements
5. **Thread-safe** - Can be cloned and shared across async tasks

## Testing
```bash
# Run circuit breaker tests
cargo test --bin synapse-core stellar::client::tests::test_circuit_breaker

# Check circuit breaker state
let state = client.circuit_state(); // Returns "closed" or "open"
```

## Usage Example
```rust
match client.get_account(address).await {
    Ok(account) => { /* Handle success */ },
    Err(HorizonError::CircuitBreakerOpen(msg)) => {
        // Circuit breaker is open - Horizon is likely down
        // Return 503 Service Unavailable
    },
    Err(e) => { /* Handle other errors */ }
}
```

## CI/CD Compatibility
- ✅ Code formatted with `cargo fmt`
- ✅ All clippy warnings resolved
- ✅ Compatible with `SQLX_OFFLINE=true` build
- ✅ Tests pass independently

## Future Enhancements
- Expose circuit breaker metrics via Prometheus (Issue #14)
- Add circuit breaker state to `/health` endpoint
- Per-endpoint circuit breakers for fine-grained control

## Resolves
Closes #18

## Checklist
- [x] Code follows project style guidelines
- [x] Tests added and passing
- [x] Documentation updated
- [x] No clippy warnings
- [x] Code formatted with `cargo fmt`
- [x] PR targets `develop` branch
