# Pull Request: Load Test Validation Suite (Issue #90)

## Summary

This PR implements automated validation of load testing results to ensure performance benchmarks are met and detect performance regressions.

## Changes

### New Files

1. **`tests/load_validation_test.rs`** (548 lines)
   - Core validation test suite
   - Parses k6 JSON output
   - Validates performance metrics against thresholds
   - Includes 10 comprehensive test cases

2. **`tests/load/run_validation.sh`** (125 lines)
   - Helper script to run load tests with JSON output
   - Automates test execution and validation
   - Supports running individual or all tests

3. **`tests/load/VALIDATION_README.md`** (285 lines)
   - Comprehensive documentation
   - Usage examples and integration guides
   - Troubleshooting tips
   - CI/CD integration examples

## Features Implemented

### Performance Validation
- ✅ P95 latency validation (< 500ms sustained, < 1000ms spike)
- ✅ P99 latency validation (< 1000ms sustained, < 2000ms spike)
- ✅ Error rate validation (< 5% sustained, < 10% spike, < 2% soak)
- ✅ Throughput validation (> 10 req/s sustained, > 5 req/s spike)
- ✅ Database connection limits (< 200 connections)
- ✅ Memory usage stability (< 1024 MB)
- ✅ CPU usage monitoring (< 90% sustained, < 95% spike)

### Test Cases

1. `test_p95_latency_under_threshold` - Validates 95th percentile latency
2. `test_p95_latency_exceeds_threshold` - Tests failure detection
3. `test_error_rate_under_threshold` - Validates error rate is acceptable
4. `test_error_rate_exceeds_threshold` - Tests error rate failure detection
5. `test_throughput_meets_minimum` - Validates minimum throughput
6. `test_db_connections_within_limits` - Validates DB connection limits
7. `test_memory_usage_stable` - Validates memory stability
8. `test_cpu_usage_reasonable` - Validates CPU usage
9. `test_percentile_calculation` - Tests percentile calculation logic
10. `test_spike_test_thresholds` - Validates spike test thresholds
11. `test_soak_test_thresholds` - Validates soak test thresholds

### Performance Thresholds

Three threshold profiles for different test scenarios:

**Sustained Load**
- P95: 500ms, P99: 1000ms
- Error rate: 5%
- Throughput: 10 req/s

**Spike Test**
- P95: 1000ms, P99: 2000ms
- Error rate: 10%
- Throughput: 5 req/s

**Soak Test**
- P95: 500ms, P99: 1000ms
- Error rate: 2% (stricter for stability)
- Throughput: 8 req/s

## Usage

### Run Load Test with Validation

```bash
# Using helper script
./tests/load/run_validation.sh callback_load

# Manual execution
docker-compose -f docker-compose.load.yml up -d app
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/results.json \
  /scripts/callback_load.js
cargo test --test load_validation_test
```

### Run Validation Tests Only

```bash
# All tests
cargo test --test load_validation_test

# Specific test
cargo test --test load_validation_test test_p95_latency_under_threshold

# With output
cargo test --test load_validation_test -- --nocapture
```

## CI/CD Integration

The validation tests are designed to integrate with CI/CD pipelines:

```yaml
- name: Run load test
  run: docker-compose -f docker-compose.load.yml run --rm k6 run \
    --out json=results.json /scripts/callback_load.js

- name: Validate results
  run: cargo test --test load_validation_test
```

## Technical Details

### k6 JSON Parsing
- Parses line-delimited JSON output from k6
- Extracts metrics: `http_req_duration`, `http_req_failed`, `http_reqs`, `errors`, `iterations`, `vus`
- Calculates percentiles (p95, p99) from raw data
- Computes error rates and throughput

### Validation Logic
- Compares metrics against configurable thresholds
- Generates detailed pass/fail reports
- Provides warnings for missing data
- Supports multiple threshold profiles

### Code Quality
- ✅ All code formatted with `cargo fmt`
- ✅ No clippy warnings
- ✅ Comprehensive test coverage
- ✅ Well-documented with inline comments
- ✅ Follows Rust best practices

## Testing

All tests pass locally:

```bash
$ cargo test --test load_validation_test
running 11 tests
test tests::test_cpu_usage_reasonable ... ok
test tests::test_db_connections_within_limits ... ok
test tests::test_error_rate_exceeds_threshold ... ok
test tests::test_error_rate_under_threshold ... ok
test tests::test_memory_usage_stable ... ok
test tests::test_p95_latency_exceeds_threshold ... ok
test tests::test_p95_latency_under_threshold ... ok
test tests::test_percentile_calculation ... ok
test tests::test_soak_test_thresholds ... ok
test tests::test_spike_test_thresholds ... ok
test tests::test_throughput_meets_minimum ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Future Enhancements

- Integration with PostgreSQL metrics for real-time connection monitoring
- Integration with container metrics for memory/CPU monitoring
- Automated regression detection (compare against baseline)
- Grafana dashboard integration
- Slack/email notifications on threshold violations
- Historical trend analysis

## Checklist

- [x] Created feature branch `feature/issue-90-load-test-validation`
- [x] Implemented all required test cases
- [x] Added comprehensive documentation
- [x] Code formatted with `cargo fmt`
- [x] All tests pass
- [x] Ready for PR against `develop` branch

## Related Issues

Closes #90

## Screenshots/Output

Example validation output:

```
=== Load Test Validation Results ===

✓ PASSED (4):
  ✓ P95 latency 380.0ms is within threshold 500.0ms
  ✓ P99 latency 396.0ms is within threshold 1000.0ms
  ✓ Error rate 3.00% is within threshold 5.00%
  ✓ Throughput 15.50 req/s meets minimum 10.00 req/s

Overall: PASS ✓
```
