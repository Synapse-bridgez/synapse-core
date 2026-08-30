# Database Connection Pool Monitoring

## Overview

The database connection pool monitoring feature provides real-time visibility into connection pool usage, enabling proactive detection of pool exhaustion issues before they cause service outages.

## Features

### 1. Health Check Integration

The `/health` endpoint now includes detailed pool statistics:

```json
{
  "status": "healthy",
  "version": "0.1.0",
  "db": "connected",
  "db_pool": {
    "active_connections": 2,
    "idle_connections": 3,
    "max_connections": 5,
    "usage_percent": 40.0
  }
}
```

### 2. Background Monitoring

A background task runs every 30 seconds to monitor pool usage:

- **Normal operation**: Logs debug-level messages with current pool status
- **High usage (≥80%)**: Logs warning messages to alert operators

Example log output:
```
2024-02-22T10:30:00Z WARN synapse_core: Database connection pool usage high: 80.0% (4/5 connections active, 1 idle)
```

### 3. Metrics Exposed

The following pool statistics are available:

- `active_connections`: Number of connections currently in use
- `idle_connections`: Number of connections available in the pool
- `max_connections`: Maximum pool size (configured in `src/db/mod.rs`)
- `usage_percent`: Percentage of pool capacity in use

### 4. Trend-Based Early-Warning (Telemetry Connection Pool)

`src/telemetry/connection_pool.rs`'s `ConnectionPool` (used for telemetry
exporter connections) additionally tracks a **leading indicator**, not just
current utilization: `ExhaustionForecaster` fits a linear trend over a
sliding window of recent utilization samples and projects it forward. This
fires *before* the pool is actually exhausted, giving operators time to
react — unlike a "pool is at 100%" alert, which only fires after requests
are already queuing or failing.

Call `ConnectionPool::forecast_exhaustion()` from a periodic health check
(alongside `total_count()`/`idle_count()`); it returns `Some(projected_utilization)`
when a *sustained* growth trend forecasts the pool crossing the warning
threshold within the forecast window, or `None` otherwise.

A spike-then-recover traffic burst nets out to a near-zero slope over the
sliding window (utilization rises, then falls back), so it does not trigger
a false positive — only a trend that holds across the whole window does.

**Default thresholds** (`EarlyWarningConfig::default()`), calibrated against
`tests/load/` steady-ramp runs:

| Setting | Default | Rationale |
|---------|---------|-----------|
| `window` | 120s | Long enough to average out a brief request-burst spike (which recovers within seconds), short enough to react to a real trend within a couple of minutes. |
| `forecast_window` | 60s | How far ahead to project; gives operators roughly a minute of lead time before actual exhaustion. |
| `warning_threshold` | 0.9 (90%) | Matches the existing "Critical" threshold in the alert table above, but as a *projected* value. |
| `min_samples` | 4 | Avoids evaluating a trend from too little data early in the pool's lifetime. |

**Calibrating per-deployment:** widen `window` if your traffic has frequent,
brief bursts that shouldn't count as a trend (fewer false positives, slower
reaction); narrow it if you need faster detection and can tolerate more
noise. Increase `forecast_window` if your on-call response time is longer
than the default's ~1 minute of lead time. Validate any change against a
`tests/load/` run reproducing your real traffic shape before deploying it.

## Configuration

Pool size is configured in `src/db/mod.rs`:

```rust
pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)  // Adjust based on workload
        .connect(&config.database_url)
        .await
}
```

## Monitoring & Alerts

### Recommended Alert Thresholds

1. **Warning**: Pool usage ≥ 80% for more than 2 minutes
2. **Critical**: Pool usage ≥ 95% or all connections exhausted

### Integration with Monitoring Systems

The health endpoint can be scraped by monitoring tools like:

- Prometheus (via custom exporter)
- Datadog
- New Relic
- CloudWatch

Example Prometheus alert rule:
```yaml
- alert: DatabasePoolHighUsage
  expr: db_pool_usage_percent > 80
  for: 2m
  labels:
    severity: warning
  annotations:
    summary: "Database connection pool usage is high"
    description: "Pool usage is {{ $value }}% on {{ $labels.instance }}"
```

## Testing

Use the provided test script to verify the feature:

```bash
./test-pool-monitoring.sh
```

Or manually test the health endpoint:

```bash
curl http://localhost:3000/health | jq '.db_pool'
```

## Troubleshooting

### High Pool Usage

If you see persistent high pool usage warnings:

1. **Check for connection leaks**: Ensure all database queries properly release connections
2. **Increase pool size**: Adjust `max_connections` in `src/db/mod.rs`
3. **Optimize queries**: Long-running queries hold connections longer
4. **Scale horizontally**: Add more application instances

### Pool Exhaustion

If the pool is completely exhausted:

1. Check application logs for slow queries
2. Review recent code changes for connection leaks
3. Monitor database performance (CPU, I/O, locks)
4. Consider implementing connection timeouts

## Implementation Details

### Files Modified

- `src/handlers/mod.rs`: Added `DbPoolStats` struct and updated health handler
- `src/main.rs`: Added background monitoring task
- `src/db/mod.rs`: Pool configuration (no changes, but relevant for tuning)

### Dependencies

Uses built-in sqlx pool methods:
- `pool.size()`: Returns active connections
- `pool.num_idle()`: Returns idle connections
- `pool.options().get_max_connections()`: Returns max pool size

No additional dependencies required.

## Future Enhancements

Potential improvements for future iterations:

1. Expose metrics via Prometheus endpoint (Issue #14)
2. Add connection wait time tracking
3. Implement dynamic pool sizing based on load
4. Add pool statistics to structured logging
5. Create dashboard templates for common monitoring tools
