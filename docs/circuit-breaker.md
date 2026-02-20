# Circuit Breaker for Stellar Horizon

## Overview

The Horizon client implements a circuit breaker pattern to protect the system from cascading failures when the Stellar Horizon API is down or slow. This prevents worker threads from piling up and crashing the application.

## How It Works

The circuit breaker has three states:

1. **Closed** (Normal): Requests pass through to Horizon API
2. **Open** (Fail Fast): After consecutive failures, the circuit opens and immediately rejects requests without calling the API
3. **Half-Open** (Probe): After a timeout period, the circuit allows a test request to check if the service has recovered

## Configuration

### Default Configuration

```rust
let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
```

- Failure threshold: 5 consecutive failures
- Reset timeout: 60 seconds
- Backoff strategy: Exponential (10s to 60s)

### Custom Configuration

```rust
use std::time::Duration;

let client = HorizonClient::with_circuit_breaker_config(
    "https://horizon-testnet.stellar.org".to_string(),
    3,                              // failure_threshold: open after 3 failures
    Duration::from_secs(30),        // reset_timeout: try again after 30s
);
```

## Error Handling

When the circuit breaker is open, API calls return:

```rust
Err(HorizonError::CircuitBreakerOpen)
```

This allows the application to:
- Return appropriate error responses to users
- Implement fallback logic
- Avoid wasting resources on doomed requests

## Implementation Details

- Uses the `failsafe` crate for circuit breaker logic
- Wraps all `HorizonClient` API calls automatically
- Thread-safe and can be cloned across async tasks
- Consecutive failures policy: opens after N consecutive failures
- Exponential backoff: gradually increases wait time between retry attempts

## Future Enhancements

- Expose circuit breaker state via metrics endpoint (see Issue #14)
- Add configurable failure predicates (e.g., only count 5xx errors)
- Implement custom instrumentation for logging state transitions
