# Load Test Validation Suite

## Description

Implements automated validation of load testing results to ensure performance benchmarks are met and detect performance regressions.

## Changes

- Created `tests/load_validation_test.rs` with 11 test cases that parse k6 JSON output
- Added `tests/load/run_validation.sh` helper script for running tests
- Added comprehensive documentation in `tests/load/VALIDATION_README.md` and `tests/load/QUICKSTART.md`

## Features

### Validates Performance Metrics

- P95 latency under threshold (500ms sustained, 1000ms spike)
- P99 latency under threshold (1000ms sustained, 2000ms spike)
- Error rate under threshold (5% sustained, 10% spike, 2% soak)
- Throughput meets minimum (10 req/s sustained, 5 req/s spike)
- Database connections within limits (200 max)
- Memory usage stable (1024 MB max)
- CPU usage reasonable (90% sustained, 95% spike)

### Test Cases

1. `test_p95_latency_under_threshold` - Validates 95th percentile latency
2. `test_p95_latency_exceeds_threshold` - Tests failure detection
3. `test_error_rate_under_threshold` - Validates error rate
4. `test_error_rate_exceeds_threshold` - Tests error detection
5. `test_throughput_meets_minimum` - Validates throughput
6. `test_db_connections_within_limits` - Validates DB limits
7. `test_memory_usage_stable` - Validates memory stability
8. `test_cpu_usage_reasonable` - Validates CPU usage
9. `test_percentile_calculation` - Tests calculation logic
10. `test_spike_test_thresholds` - Validates spike thresholds
11. `test_soak_test_thresholds` - Validates soak thresholds

## Usage

```bash
# Run load test with JSON output
docker-compose -f docker-compose.load.yml up -d app
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=results.json /scripts/callback_load.js

# Validate results
cargo test --test load_validation_test

# Or use helper script
./tests/load/run_validation.sh callback_load
```

## Testing

All tests pass:

```bash
cargo test --test load_validation_test
# running 11 tests
# test result: ok. 11 passed; 0 failed
```

Code formatted and ready for CI:

```bash
cargo fmt --check  # ✓ passes
```

## Documentation

- `tests/load/VALIDATION_README.md` - Full documentation with CI/CD examples
- `tests/load/QUICKSTART.md` - 5-minute getting started guide
- Inline code documentation and comments

## Closes

Issue #90
