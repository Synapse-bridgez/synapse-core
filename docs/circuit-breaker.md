# Circuit Breaker for Stellar Horizon

## Overview

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

## Redis-backed breaker and the half-open probe

`src/services/circuit_breaker.rs` contains a separate, generic `CircuitBreaker` that wraps any async call with `call()`. It keeps its state in memory and persists it to Redis (key `cb:state:<service_name>`) when it opens, so a restart does not forget that a dependency is down. It is built with a service name, a Redis client, a failure threshold and a reset timeout.

### State transitions

| From      | Event                                              | To        |
| --------- | -------------------------------------------------- | --------- |
| Closed    | A call fails and the failure count reaches the threshold | Open |
| Open      | A call arrives before `reset_timeout` has elapsed  | Open (the call is rejected with `CircuitBreakerError::Open` and `f` is not run) |
| Open      | A call arrives after `reset_timeout` has elapsed   | Half-Open (that call runs as the probe) |
| Half-Open | The probe succeeds                                 | Closed (failure count, `opened_at` and `last_error` are cleared) |
| Half-Open | The probe fails                                    | Open (`opened_at` is reset, so a new `reset_timeout` starts) |

### How the probe works

- There is no separate probe request. The first call that arrives after the reset timeout moves the breaker to Half-Open and is executed as the probe.
- The failure count is not cleared when entering Half-Open, so it is still at or above the threshold. Any failure of the probe therefore re-opens the breaker immediately and writes the new state to Redis.
- The breaker does not limit concurrency in Half-Open: calls that arrive while the probe is in flight also run. Their results update the same state, and the last result to finish decides whether the breaker ends up Closed or Open.
- The persisted state expires after `reset_timeout` seconds, so a stale Open record is dropped on its own after a restart.
- `load_from_redis()` restores a persisted state; call it at startup if the state should survive restarts.
- `get_state()` returns a snapshot (`state`, `opened_at`, `failure_count`, `last_error`) for health checks and metrics.

See [ADR-002](adr/002-circuit-breaker.md) for the original design.
