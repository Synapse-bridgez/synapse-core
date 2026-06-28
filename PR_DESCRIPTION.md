# PR: Telemetry Rate Limiting

**Branch**: `feature/telemetry-rate-limiting`

## Summary

Adds per-event-type token-bucket rate limiting for telemetry operations (traces, metrics, and events). Introduces `TelemetryRateLimiter` that reuses the lock-free `RateLimiter` from `crate::cache::rate_limiting`, with independent buckets for each event type. All rate-limit overflows are non-fatal — events are dropped, a warning is logged, and rejection metrics are recorded.

## Problem

The telemetry module previously had no rate limiting. A burst of traces, metrics, or log events could overwhelm the export pipeline, causing backpressure to spike and potentially degrade application performance. Without isolation between event types, a burst in one category (e.g., metrics) could starve others (e.g., traces).

## Solution

### New Module: `src/telemetry/rate_limiting.rs`

- **`TelemetryRateLimitConfig`** — Configurable per-event-type limits and shared window duration.
- **`TelemetryRateLimiter`** — O(1)-cloneable struct wrapping three independent `RateLimiter` buckets (trace, metric, event).
- **`TelemetryRateLimitMetrics`** — Snapshot of acquired/rejected counts per event type for observability.
- Reuses the existing lock-free token-bucket implementation from `crate::cache::rate_limiting` to avoid duplicating rate-limit logic.

### Key Behaviors

| Event type | Default limit | Window |
|------------|--------------|--------|
| Trace      | 1000         | 60 s   |
| Metric     | 5000         | 60 s   |
| Event      | 500          | 60 s   |

- **Non-fatal**: exceeded limits drop the event and emit a `tracing::warn!` — no panic.
- **Independent buckets**: one type cannot starve another.
- **Metrics**: `metrics()` returns per-type acquired/rejected counts.
- **Reset**: `reset_all()` restores all buckets (useful for tests or operator intervention).
- **Exhaustion check**: `any_exhausted()` supports health-check / backpressure signaling.

## Changes

### Created Files
- ✅ `src/telemetry/rate_limiting.rs` — New rate-limiting module for telemetry

### Modified Files

| File | Changes |
|------|---------|
| `src/telemetry/mod.rs` | Export `rate_limiting` module and public types |

## API

```rust
use synapse_core::telemetry::{TelemetryRateLimiter, TelemetryRateLimitConfig, TelemetryRateLimitMetrics};

let limiter = TelemetryRateLimiter::new();

if limiter.try_acquire_trace() {
    // process trace
} else {
    // dropped — warning already logged
}

let metrics: TelemetryRateLimitMetrics = limiter.metrics();
```

## Testing

### Unit Tests (37 tests)
```bash
cargo test --lib telemetry::rate_limiting::tests
```

Coverage includes:
- Default and custom configuration
- Per-type acquire/reject paths
- Independent bucket isolation
- `try_acquire(&RecordType)` dispatch
- Remaining-token accounting
- Metrics snapshot accuracy
- `reset_all()` restoration
- `any_exhausted()` boolean logic
- Clone shares state (O(1) Arc semantics)
- Edge cases: zero limits, large windows, default-trait equivalence

## Backward Compatibility

✅ **No breaking changes**
- All existing telemetry APIs are unchanged.
- New module is opt-in; existing callers compile without modification.

## Code Review Notes

- Minimal implementation: only types and logic necessary for telemetry rate limiting.
- No duplicated token-bucket algorithm — delegates to `crate::cache::rate_limiting::RateLimiter`.
- All overflow paths are logged and measured, never panicking.
- Follows existing codebase conventions (module layout, doc comments, test naming).
