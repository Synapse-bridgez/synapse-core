# Load Testing Suite

Comprehensive k6-based load testing for the Synapse Core API.

## Quick Start

```bash
# Start the load test environment
docker-compose -f docker-compose.load.yml up -d app

# Run a specific test
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js

# Run all tests
./run-load-tests.sh

# Cleanup
docker-compose -f docker-compose.load.yml down -v
```

## Test Scenarios

### 1. `callback_load.js` - Sustained Load Test
Gradually increases load from 10 → 50 → 100 concurrent users over 23 minutes.

**Purpose**: Establish baseline performance metrics and identify when degradation begins.

**Thresholds**:
- p95 latency < 500ms
- p99 latency < 1000ms
- Error rate < 5%

### 2. `spike_test.js` - Spike Test
Simulates sudden traffic surge (10 → 200 users in 30 seconds).

**Purpose**: Test system resilience during unexpected traffic spikes (e.g., batch webhook delivery).

**Thresholds**:
- p95 latency < 1000ms
- Error rate < 10%

### 3. `soak_test.js` - Soak Test
Maintains 30 concurrent users for 30 minutes.

**Purpose**: Identify memory leaks, connection leaks, and long-term stability issues.

**Thresholds**:
- p95 latency < 500ms (should remain stable)
- Error rate < 2%

### 4. `idempotency_test.js` - Idempotency Test
Tests duplicate webhook detection with 50% shared idempotency keys.

**Purpose**: Verify idempotency middleware performance and Redis caching effectiveness.

**Thresholds**:
- p95 latency < 300ms
- Idempotency hit rate > 30%

## Environment Variables

- `BASE_URL`: Target API URL (default: `http://localhost:3000`)

Example:
```bash
docker-compose -f docker-compose.load.yml run --rm -e BASE_URL=http://app:3000 k6 run /scripts/callback_load.js
```

## Monitoring During Tests

```bash
# Monitor container resources
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Check database connections
docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"

# Check Redis memory usage
docker exec synapse-load-redis redis-cli INFO memory

# View app logs
docker logs -f synapse-load-app
```

## Interpreting Results

k6 outputs detailed metrics after each test:

```
http_req_duration.............: avg=XXXms min=XXXms med=XXXms max=XXXms p(95)=XXXms p(99)=XXXms
http_req_failed...............: XX.XX%
http_reqs.....................: XXXXX
iterations....................: XXXXX
vus...........................: XXX
```

Key metrics to watch:
- **p95/p99 latency**: Should stay within thresholds
- **http_req_failed**: Error rate percentage
- **Custom metrics**: `errors`, `webhook_duration`, `idempotency_hits`

## Troubleshooting

### Tests fail immediately
- Ensure services are healthy: `docker-compose -f docker-compose.load.yml ps`
- Check app logs: `docker logs synapse-load-app`

### High error rates
- Check database connection pool size
- Monitor CPU/memory usage
- Review app logs for errors

### Slow response times
- Check database query performance
- Monitor Redis latency
- Review connection pool utilization

## Next Steps

1. Run baseline tests and document results in `docs/load-test-results.md`
2. Identify bottlenecks from metrics
3. Optimize configuration (DB pool, Redis, etc.)
4. Re-run tests to validate improvements
5. Set up continuous load testing in CI/CD
