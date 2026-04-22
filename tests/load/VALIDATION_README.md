# Load Test Validation

Automated validation of load testing results to ensure performance benchmarks are met and detect performance regressions.

## Overview

This validation suite parses k6 JSON output and validates that:
- P95 latency is under threshold
- P99 latency is under threshold  
- Error rate is under threshold
- Throughput meets minimum requirements
- Database connections stay within limits
- Memory usage is stable
- CPU usage is reasonable

## Quick Start

### 1. Run Load Tests with JSON Output

```bash
# Start the load test environment
docker-compose -f docker-compose.load.yml up -d app

# Run a test with JSON output
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/callback_load_results.json \
  /scripts/callback_load.js

# Cleanup
docker-compose -f docker-compose.load.yml down
```

### 2. Run Validation Tests

```bash
# Run all validation tests
cargo test --test load_validation_test

# Run specific validation test
cargo test --test load_validation_test test_p95_latency_under_threshold

# Run with output
cargo test --test load_validation_test -- --nocapture
```

### 3. Use the Helper Script

```bash
# Run sustained load test
./tests/load/run_validation.sh callback_load

# Run spike test
./tests/load/run_validation.sh spike_test

# Run all tests (except soak test)
./tests/load/run_validation.sh all
```

## Test Cases

### Core Validation Tests

#### `test_p95_latency_under_threshold`
Validates that 95th percentile latency is below the configured threshold.

**Threshold**: 500ms for sustained load, 1000ms for spike tests

**Example**:
```rust
let thresholds = PerformanceThresholds::sustained_load();
assert!(load_metrics.http_req_duration_p95.unwrap() < thresholds.p95_latency_ms);
```

#### `test_error_rate_under_threshold`
Validates that the error rate (failed requests / total requests) is below threshold.

**Threshold**: 5% for sustained load, 10% for spike tests, 2% for soak tests

**Example**:
```rust
let thresholds = PerformanceThresholds::sustained_load();
assert!(load_metrics.http_req_failed_rate.unwrap() < thresholds.error_rate_percent);
```

#### `test_throughput_meets_minimum`
Validates that the system achieves minimum throughput (requests per second).

**Threshold**: 10 req/s for sustained load, 5 req/s for spike tests

**Example**:
```rust
let thresholds = PerformanceThresholds::sustained_load();
assert!(load_metrics.http_reqs_per_second.unwrap() >= thresholds.min_throughput_rps);
```

#### `test_db_connections_within_limits`
Validates that database connections stay within configured limits.

**Threshold**: 200 max connections

**Note**: This test demonstrates the concept. In production, integrate with PostgreSQL metrics:
```sql
SELECT count(*) FROM pg_stat_activity WHERE datname = 'synapse';
```

#### `test_memory_usage_stable`
Validates that memory usage remains stable and doesn't grow unbounded.

**Threshold**: 1024 MB max

**Note**: This test demonstrates the concept. In production, integrate with container metrics:
```bash
docker stats synapse-load-app --no-stream --format "{{.MemUsage}}"
```

#### `test_cpu_usage_reasonable`
Validates that CPU usage stays within reasonable limits.

**Threshold**: 90% max for sustained load, 95% for spike tests

**Note**: This test demonstrates the concept. In production, integrate with container metrics:
```bash
docker stats synapse-load-app --no-stream --format "{{.CPUPerc}}"
```

## Performance Thresholds

### Sustained Load Test (`PerformanceThresholds::sustained_load()`)
```rust
p95_latency_ms: 500.0
p99_latency_ms: 1000.0
error_rate_percent: 5.0
min_throughput_rps: 10.0
max_db_connections: 200
max_memory_mb: 1024.0
max_cpu_percent: 90.0
```

### Spike Test (`PerformanceThresholds::spike_test()`)
More lenient thresholds for sudden traffic surges:
```rust
p95_latency_ms: 1000.0
p99_latency_ms: 2000.0
error_rate_percent: 10.0
min_throughput_rps: 5.0
max_db_connections: 200
max_memory_mb: 1024.0
max_cpu_percent: 95.0
```

### Soak Test (`PerformanceThresholds::soak_test()`)
Stricter error rate for stability testing:
```rust
p95_latency_ms: 500.0
p99_latency_ms: 1000.0
error_rate_percent: 2.0
min_throughput_rps: 8.0
max_db_connections: 200
max_memory_mb: 1024.0
max_cpu_percent: 85.0
```

## k6 JSON Output Format

The validation tests parse k6 JSON output in the following format:

```json
{"type":"Point","data":{"time":"1234567890","value":123.45,"tags":{}},"metric":"http_req_duration"}
{"type":"Point","data":{"time":"1234567891","value":0,"tags":{}},"metric":"http_req_failed"}
{"type":"Point","data":{"time":"1234567892","value":1,"tags":{}},"metric":"http_reqs"}
```

### Supported Metrics

- `http_req_duration`: Request duration in milliseconds (used for p95/p99 calculation)
- `http_req_failed`: Binary indicator (1 = failed, 0 = success)
- `http_reqs`: Request counter
- `errors`: Custom error counter
- `iterations`: Test iteration counter
- `vus`: Virtual users (concurrent users)

## Integration with CI/CD

### GitHub Actions Example

```yaml
name: Load Test Validation

on:
  schedule:
    - cron: '0 2 * * *'  # Run nightly at 2 AM
  workflow_dispatch:

jobs:
  load-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      
      - name: Start services
        run: docker-compose -f docker-compose.load.yml up -d app
      
      - name: Wait for services
        run: sleep 10
      
      - name: Run load test
        run: |
          docker-compose -f docker-compose.load.yml run --rm k6 run \
            --out json=test_results/results.json \
            /scripts/callback_load.js
      
      - name: Validate results
        run: cargo test --test load_validation_test
      
      - name: Upload results
        uses: actions/upload-artifact@v3
        with:
          name: load-test-results
          path: test_results/
      
      - name: Cleanup
        run: docker-compose -f docker-compose.load.yml down -v
```

## Customizing Thresholds

To customize thresholds for your environment, modify the threshold values:

```rust
let mut thresholds = PerformanceThresholds::sustained_load();
thresholds.p95_latency_ms = 300.0;  // Stricter latency requirement
thresholds.error_rate_percent = 1.0;  // Stricter error rate
```

Or create custom thresholds:

```rust
let thresholds = PerformanceThresholds {
    p95_latency_ms: 250.0,
    p99_latency_ms: 500.0,
    error_rate_percent: 1.0,
    min_throughput_rps: 50.0,
    max_db_connections: 100,
    max_memory_mb: 512.0,
    max_cpu_percent: 80.0,
};
```

## Troubleshooting

### Test fails with "data not available"
The k6 test may not have generated enough data points. Ensure:
- The test ran successfully
- The JSON output file exists and is not empty
- The test duration was sufficient to generate metrics

### High error rates
Check:
- Database connection pool size
- Redis availability
- Application logs for errors
- System resource limits (CPU, memory)

### High latency
Investigate:
- Database query performance
- Connection pool exhaustion
- Network latency
- CPU/memory pressure

## Future Enhancements

- [ ] Integration with PostgreSQL metrics for real-time connection monitoring
- [ ] Integration with container metrics for memory/CPU monitoring
- [ ] Automated regression detection (compare against baseline)
- [ ] Grafana dashboard integration
- [ ] Slack/email notifications on threshold violations
- [ ] Historical trend analysis
- [ ] Performance budget enforcement in CI/CD

## References

- [k6 Documentation](https://k6.io/docs/)
- [k6 Metrics](https://k6.io/docs/using-k6/metrics/)
- [Load Testing Best Practices](https://k6.io/docs/testing-guides/load-testing-best-practices/)
