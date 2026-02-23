## Connection Draining for Zero-Downtime Deployments

### Summary
Implements connection draining to enable zero-downtime rolling deployments by allowing in-flight requests to complete before shutdown.

### Changes
- Added `/ready` endpoint (separate from `/health`) for Kubernetes readiness probes
- Returns 200 when accepting traffic, 503 during drain
- On SIGTERM: immediately stops accepting new connections, waits up to 30s for in-flight requests

### New Files
- `src/readiness.rs` - AtomicBool-based readiness state
- `tests/readiness_unit_test.rs` - Unit tests
- `tests/connection_draining_test.rs` - Integration tests

### Modified Files  
- `src/config.rs` - Added `DRAIN_TIMEOUT_SECS` config
- `src/handlers/mod.rs` - Added `/ready` handler
- `src/lib.rs` - Wired readiness into AppState
- `src/main.rs` - Added SIGTERM handler with graceful shutdown

### Configuration
- `DRAIN_TIMEOUT_SECS` - Drain timeout in seconds (default: 30)
