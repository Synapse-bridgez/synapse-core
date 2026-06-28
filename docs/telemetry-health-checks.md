# Telemetry Health Checks

This document describes the health checks implemented in the telemetry module to ensure resilient and safe operation of the OpenTelemetry exporter.

## Overview

The telemetry module implements four complementary health check strategies to monitor the health of the OpenTelemetry exporter and gracefully degrade when the exporter is unavailable:

1. **Connection Pool Health** — Monitors resource usage and prevents exhaustion.
2. **Exporter Connectivity** — Detects and recovers from transient failures.
3. **Error Handling** — Determines recovery strategy based on error severity.
4. **Input Validation** — Rejects malformed data at the source.

## Connection Pool Health

The [`ConnectionPool`](../src/telemetry/connection_pool.rs) enforces a hard cap on the number of connections to the telemetry exporter, preventing resource exhaustion attacks.

### What It Checks

- **Pool Capacity**: Ensures the pool size never exceeds `max_size`. Returns `PoolError::Exhausted` when all connections are in use.
- **Idle Timeout**: Evicts idle connections exceeding `max_idle` duration on the next pool operation, preventing unbounded resource hold.
- **Endpoint Validation**: Validates the exporter endpoint URL at initialization time, rejecting invalid schemes (non-http/https) and over-long URLs.

### Configuration

```rust
use synapse_core::telemetry::PoolConfig;
use std::time::Duration;

let config = PoolConfig {
    max_size: 10,                              // Hard cap: 10 connections
    max_idle: Duration::from_secs(300),        // 5-minute idle timeout
    endpoint: "https://otel-collector:4317".to_string(),
};
```

### Passing Result

A healthy pool means:
- `acquire()` returns a connection (new or idle).
- `idle_count()` reflects the number of reusable connections.
- `total_count()` is less than `max_size`.

### Failing Result

Pool exhaustion (`PoolError::Exhausted`) indicates the exporter is slow or unavailable and cannot handle the request volume. The caller should:
- Back off and retry, or
- Fail the telemetry operation gracefully (no-op degradation).

## Exporter Connectivity (Circuit Breaker)

The [`ReconnectionManager`](../src/telemetry/reconnection.rs) monitors exporter connectivity using exponential backoff and circuit breaker pattern.

### What It Checks

- **Failure Tracking**: Counts consecutive failures. When `max_failures` is reached, the circuit opens.
- **Circuit Breaker**: When open, all export attempts are rejected immediately (fail-fast) to prevent hammering a failing exporter.
- **Auto-Recovery**: The circuit automatically closes after `circuit_open_duration` elapses, allowing the exporter time to recover.
- **Backoff Strategy**: Calculates exponential backoff with jitter to space retry attempts.

### Configuration

```rust
use synapse_core::telemetry::ReconnectionConfig;
use std::time::Duration;

let config = ReconnectionConfig {
    initial_backoff: Duration::from_millis(100),  // Start with 100ms
    max_backoff: Duration::from_secs(30),         // Cap at 30s
    backoff_multiplier: 2.0,                      // Double each time
    max_failures: 5,                              // Open circuit after 5 failures
    circuit_open_duration: Duration::from_secs(60), // Auto-reset after 60s
};
```

### Passing Result

A healthy circuit breaker means:
- `is_circuit_open()` returns false.
- `failure_count()` is less than `max_failures`.
- `next_backoff()` is non-zero only if there are recent failures.

### Failing Result

Circuit open (`CircuitBreakerOpen`) indicates the exporter is unavailable. The caller should:
- Fail the telemetry operation immediately (no-op degradation).
- Wait for the circuit to auto-reset and retry later.

## Error Handling

The [`ErrorHandler`](../src/telemetry/error_handling.rs) monitors telemetry operation errors and determines whether to continue or fail based on error type and threshold.

### What It Checks

- **Error Classification**: Distinguishes between fatal errors (validation, circuit breaker) and transient errors (network, timeout).
- **Error Threshold**: Tracks consecutive errors and switches from `Continue` to `Stop` when threshold is exceeded.
- **Graceful Degradation**: Returns `Continue` for transient errors under threshold, allowing the application to proceed with no-op telemetry.

### Configuration

```rust
use synapse_core::telemetry::ErrorHandler;

// Default: tolerate up to 10 errors
let handler = ErrorHandler::new();

// Strict: fail fast on any error
let handler = ErrorHandler::fail_fast();

// Custom: tolerate up to 5 errors
let handler = ErrorHandler::with_threshold(5);
```

### Passing Result

A healthy error handler means:
- `handle_error()` returns `Continue` (transient errors under threshold).
- `threshold_exceeded()` returns false.
- `error_count()` is less than `error_threshold`.

### Failing Result

Error threshold exceeded or circuit breaker open should trigger graceful degradation. The caller should:
- Degrade to no-op telemetry (skip tracing, metrics).
- Reset the handler after a successful operation.

## Input Validation

The [`InputValidator`](../src/telemetry/input_validation.rs) validates telemetry data before export, rejecting malformed or malicious input.

### What It Checks

- **Span Name Validation**: Non-empty, ≤1024 chars, alphanumeric/underscore/hyphen/dot only.
- **Attribute Validation**: No null bytes, ≤1024 chars each.
- **Collection Size**: ≤128 attributes per span (prevents DoS).
- **Endpoint Validation**: http/https scheme only, ≤2048 chars (prevents SSRF).

### Passing Result

Valid input means the data is safe to export:

```rust
use synapse_core::telemetry::InputValidator;

InputValidator::validate_span_name("http.request").unwrap(); // OK
InputValidator::validate_endpoint("https://collector:4317").unwrap(); // OK
```

### Failing Result

Invalid input should be logged and the data dropped:

```rust
if let Err(e) = InputValidator::validate_span_name(&span_name) {
    tracing::warn!(error = %e, "Invalid span name; dropping span");
    return;
}
```

## Integration with /health Endpoint

The `/health` endpoint checks all health aspects:

- **Connection Pool**: `health::check_pool()` verifies pool is initialized and under capacity.
- **Circuit Breaker**: `health::check_exporter()` verifies the circuit is not open.
- **Error Handler**: `health::check_telemetry_errors()` verifies error threshold not exceeded.

A healthy telemetry system returns 200 OK; a degraded system returns 503 with details.

## Graceful Degradation

When the exporter is unavailable (circuit open, pool exhausted, or error threshold exceeded), the application should degrade gracefully:

```rust
match reconnection_manager.is_circuit_open() {
    true => {
        // Exporter unavailable; use no-op tracer
        tracing::warn!("Telemetry exporter unavailable; running in no-op mode");
        let tracer = no_op_tracer();
        // Continue business logic with tracer
    }
    false => {
        // Exporter available; use normal tracer
        let tracer = otel_tracer();
    }
}
```

## Adding a New Telemetry Health Check

To add a new health check:

1. **Define the check logic** in the telemetry module (e.g., new file `src/telemetry/new_check.rs`).
2. **Implement a public struct or function** that can be queried (e.g., `pub fn check_foo() -> CheckResult`).
3. **Export from `mod.rs`** so the check is part of the public API.
4. **Document with `/// Health Check`** section in all public APIs.
5. **Add to `/health` endpoint** by calling the check function and aggregating results.
6. **Test thoroughly**:
   - Happy path: check passes when conditions are met.
   - Failure path: check fails appropriately on error.
   - Recovery: check transitions from fail to pass as conditions normalize.

### Example: Adding a Memory Health Check

```rust
// src/telemetry/memory_check.rs
/// Configuration for memory health check
pub struct MemoryCheckConfig {
    pub max_usage_mb: usize,
}

/// Monitors memory usage of the telemetry exporter
pub struct MemoryChecker {
    config: MemoryCheckConfig,
}

impl MemoryChecker {
    /// Checks if memory usage is within limits.
    ///
    /// # Health Check
    ///
    /// Returns Ok if memory usage is below `max_usage_mb`, Err otherwise.
    pub fn check(&self) -> Result<(), String> {
        let usage = get_memory_usage();
        if usage > self.config.max_usage_mb {
            Err(format!("Memory usage {} MB exceeds limit {}", usage, self.config.max_usage_mb))
        } else {
            Ok(())
        }
    }
}

// src/telemetry/mod.rs
pub mod memory_check;
pub use memory_check::MemoryChecker;
```

## Observability

Telemetry health checks emit logs and metrics for observability:

- **Logs**: Each health check failure is logged at `warn!` or `error!` level.
- **Metrics**: Counter `telemetry.health.check_failures` tracks health check failures by type.
- **Traces**: Circuit breaker state transitions are traced for debugging.

Monitor the `/health` endpoint and these logs to detect exporter degradation early.

## References

- [`ConnectionPool`](../src/telemetry/connection_pool.rs) — Connection pool health.
- [`ReconnectionManager`](../src/telemetry/reconnection.rs) — Exporter connectivity.
- [`ErrorHandler`](../src/telemetry/error_handling.rs) — Error handling strategy.
- [`InputValidator`](../src/telemetry/input_validation.rs) — Input validation.
