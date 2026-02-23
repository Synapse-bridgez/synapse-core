# Pull Request: Load Testing Suite Implementation

## Summary

Implemented a comprehensive k6-based load testing infrastructure for the Synapse Core API to benchmark performance under realistic traffic patterns and identify bottlenecks before production deployment.

## What's Included

### Test Scenarios (tests/load/)
1. **callback_load.js** - Sustained Load Test
   - Gradually ramps from 10 → 50 → 100 concurrent users
   - Duration: ~23 minutes
   - Measures baseline performance and degradation points
   - Thresholds: p95 < 500ms, p99 < 1000ms, errors < 5%

2. **spike_test.js** - Traffic Surge Test
   - Simulates sudden spike: 10 → 200 users in 30 seconds
   - Duration: ~6.5 minutes
   - Tests resilience during batch webhook delivery
   - Thresholds: p95 < 1000ms, errors < 10%

3. **soak_test.js** - Stability Test
   - Maintains 30 concurrent users for 30 minutes
   - Duration: ~40 minutes
   - Identifies memory leaks and long-term stability issues
   - Thresholds: p95 < 500ms (stable), errors < 2%

4. **idempotency_test.js** - Cache Performance Test
   - Tests duplicate detection with 50% shared keys
   - Duration: ~7 minutes
   - Validates Redis caching effectiveness
   - Thresholds: p95 < 300ms, cache hit rate > 30%

5. **health_check.js** - Infrastructure Verification
   - Simple test to verify setup works
   - Duration: ~2 minutes
   - Tests /health endpoint

### Infrastructure (docker-compose.load.yml)
- **PostgreSQL**: Optimized with increased connections (200), shared_buffers (256MB)
- **Redis**: Configured with 512MB memory, LRU eviction, persistence disabled for performance
- **App**: Resource-limited (2 CPU, 1GB RAM) to simulate realistic constraints
- **k6**: Grafana k6 container for running load tests

### Documentation
- **docs/load-test-results.md**: Template for documenting baseline metrics and bottleneck analysis
- **tests/load/README.md**: Detailed test documentation with usage examples
- **LOAD_TESTING_GUIDE.md**: Quick reference for running tests
- **LOAD_TEST_STATUS.md**: Current status and next steps
- **run-load-tests.sh**: Automated script to run full test suite

## Usage

### Quick Start
```bash
# Run all tests (takes ~76 minutes)
./run-load-tests.sh

# Or run individual tests
docker-compose -f docker-compose.load.yml up -d app
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
```

### Monitoring During Tests
```bash
# Monitor resources
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Check DB connections
docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"

# Check Redis memory
docker exec synapse-load-redis redis-cli INFO memory
```

## Expected Outcomes

After running tests, you'll have:
- Baseline performance metrics (p50, p95, p99 latencies)
- Maximum sustainable throughput (requests/second)
- Breaking points and bottlenecks identified
- Resource utilization patterns
- Recommendations for production sizing

## Current Status

⚠️ **Note**: The application currently has compilation errors that need to be resolved before load tests can run against the webhook endpoints. The infrastructure is complete and ready to use once the app builds successfully.

The health check test can verify the infrastructure setup works independently.

## Files Changed

- `docker-compose.load.yml` - Load testing infrastructure
- `tests/load/*.js` - 5 k6 test scenarios
- `docs/load-test-results.md` - Results documentation template
- `run-load-tests.sh` - Automated test runner
- `LOAD_TESTING_GUIDE.md` - Quick reference
- `LOAD_TEST_STATUS.md` - Status tracking
- `dockerfile` - Fixed cargo build command

## Next Steps

1. Fix application compilation errors
2. Run baseline load tests
3. Document results in `docs/load-test-results.md`
4. Identify and address bottlenecks
5. Re-run tests to validate improvements
6. Set up continuous load testing in CI/CD

## Testing Checklist

- [x] k6 test scripts created
- [x] Docker infrastructure configured
- [x] Documentation written
- [x] Helper scripts created
- [ ] Application builds successfully
- [ ] Baseline tests executed
- [ ] Results documented
- [ ] Bottlenecks identified

Closes #29
