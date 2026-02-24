# Add Comprehensive Load Testing Suite

## Description
Implements a complete k6-based load testing infrastructure to benchmark the Synapse Core API under realistic traffic patterns and identify performance bottlenecks before production deployment.

## Changes
- ✅ Created 5 k6 test scenarios covering different load patterns
- ✅ Set up optimized Docker infrastructure for load testing
- ✅ Added comprehensive documentation and helper scripts
- ✅ Fixed dockerfile cargo build command

## Test Scenarios

### 1. Sustained Load Test (`callback_load.js`)
- Gradually ramps: 10 → 50 → 100 concurrent users
- Duration: ~23 minutes
- **Thresholds**: p95 < 500ms, p99 < 1000ms, errors < 5%

### 2. Spike Test (`spike_test.js`)
- Sudden surge: 10 → 200 users in 30 seconds
- Duration: ~6.5 minutes
- **Thresholds**: p95 < 1000ms, errors < 10%

### 3. Soak Test (`soak_test.js`)
- Sustained: 30 users for 30 minutes
- Duration: ~40 minutes
- **Thresholds**: p95 < 500ms (stable), errors < 2%

### 4. Idempotency Test (`idempotency_test.js`)
- Tests cache with 50% duplicate keys
- Duration: ~7 minutes
- **Thresholds**: p95 < 300ms, cache hit rate > 30%

### 5. Health Check Test (`health_check.js`)
- Infrastructure verification
- Duration: ~2 minutes

## Infrastructure

**docker-compose.load.yml** includes:
- PostgreSQL with optimized settings (200 connections, 256MB shared_buffers)
- Redis with performance tuning (512MB memory, LRU eviction)
- App container with resource limits (2 CPU, 1GB RAM)
- k6 container for test execution

## Documentation

- `docs/load-test-results.md` - Template for documenting results
- `tests/load/README.md` - Detailed test documentation
- `LOAD_TESTING_GUIDE.md` - Quick reference guide
- `LOAD_TEST_STATUS.md` - Current status and blockers
- `run-load-tests.sh` - Automated test runner

## Usage

```bash
# Run all tests (~76 minutes total)
./run-load-tests.sh

# Or run individual tests
docker-compose -f docker-compose.load.yml up -d app
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/spike_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/idempotency_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/soak_test.js

# Cleanup
docker-compose -f docker-compose.load.yml down -v
```

## Monitoring

```bash
# Monitor container resources
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Check database connections
docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"

# Check Redis memory
docker exec synapse-load-redis redis-cli INFO memory
```

## Expected Outcomes

After running tests, you'll have:
- ✅ Baseline performance metrics (p50, p95, p99 latencies)
- ✅ Maximum sustainable throughput (requests/second)
- ✅ Breaking points and bottlenecks identified
- ✅ Resource utilization patterns
- ✅ Production sizing recommendations

## Current Status

⚠️ **Note**: The application currently has compilation errors that need to be resolved before load tests can run against webhook endpoints. The infrastructure is complete and ready to use once the app builds successfully.

The health check test can verify the infrastructure setup independently.

## Files Added/Modified

```
docker-compose.load.yml
dockerfile (fixed)
run-load-tests.sh
tests/load/
  ├── callback_load.js
  ├── spike_test.js
  ├── soak_test.js
  ├── idempotency_test.js
  ├── health_check.js
  └── README.md
docs/load-test-results.md
LOAD_TESTING_GUIDE.md
LOAD_TEST_STATUS.md
PR_SUMMARY.md
```

## Next Steps

1. Merge this PR to get load testing infrastructure in place
2. Fix application compilation errors
3. Run baseline load tests
4. Document results in `docs/load-test-results.md`
5. Identify and address bottlenecks
6. Set up continuous load testing in CI/CD

## Testing Checklist

- [x] k6 test scripts created and validated
- [x] Docker infrastructure configured
- [x] Documentation complete
- [x] Helper scripts created
- [ ] Application builds successfully (blocker)
- [ ] Baseline tests executed
- [ ] Results documented
- [ ] Bottlenecks identified

Closes #29
