# Circuit Breaker for Stellar Horizon

## Overview

<<<<<<< HEAD
The Stellar Horizon client now includes a circuit breaker pattern to protect the system from cascading failures when the Horizon API is down or experiencing issues.

## How It Works

The circuit breaker has three states:

1. **Closed** (Normal): Requests pass through to Horizon API
2. **Open** (Fail Fast): Requests are immediately rejected without calling the API
3. **Half-Open** (Probing): After a timeout, the circuit allows test requests to check if the service has recovered

## Configuration

### Default Configuration

```rust
let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
```

Default settings:
- **Failure Threshold**: 3 consecutive failures
- **Reset Timeout**: 60-120 seconds (with jitter)

### Custom Configuration

```rust
let client = HorizonClient::with_circuit_breaker(
    "https://horizon-testnet.stellar.org".to_string(),
    5,    // failure_threshold: number of consecutive failures before opening
    30,   // reset_timeout_secs: seconds to wait before attempting recovery
);
```

## Usage

The circuit breaker is transparent to the caller:

```rust
match client.get_account(address).await {
    Ok(account) => {
        // Handle successful response
    },
    Err(HorizonError::CircuitBreakerOpen(msg)) => {
        // Circuit breaker is open - Horizon is likely down
        // Return 503 Service Unavailable or retry later
    },
    Err(HorizonError::RequestError(e)) => {
        // Network or HTTP error
    },
    Err(HorizonError::AccountNotFound(addr)) => {
        // Account doesn't exist
    },
    Err(e) => {
        // Other errors
    }
}
```

## Monitoring

Check the circuit breaker state:

```rust
let state = client.circuit_state();
// Returns: "closed" or "open"
```

This can be exposed via metrics or health check endpoints.

## Benefits

1. **Prevents Resource Exhaustion**: Worker threads don't pile up waiting for timeouts
2. **Fast Failure**: Immediate rejection when the service is known to be down
3. **Automatic Recovery**: Automatically probes for service recovery
4. **Configurable**: Adjust thresholds based on your requirements

## Implementation Details

- Uses the `failsafe` crate for circuit breaker logic
- Failure policy: Consecutive failures (not percentage-based)
- Backoff strategy: Equal jittered (prevents thundering herd)
- Thread-safe: Can be cloned and shared across async tasks

## Testing

Run the circuit breaker tests:

```bash
cargo test --bin synapse-core stellar::client::tests::test_circuit_breaker
```

## Future Enhancements

- Expose circuit breaker metrics via Prometheus (see Issue #14)
- Add circuit breaker state to `/health` endpoint
- Configurable failure policies (e.g., percentage-based)
- Per-endpoint circuit breakers for fine-grained control
||||||| 2822865
=======
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
>>>>>>> refs/remotes/origin/feature/issue-18-circuit-breaker
