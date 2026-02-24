# Load Testing Quick Reference

## Running Tests

### Option 1: Run All Tests (Recommended for first run)
```bash
./run-load-tests.sh
```
Total duration: ~76 minutes

### Option 2: Run Individual Tests

```bash
# Start infrastructure
docker-compose -f docker-compose.load.yml up -d app

# Run specific test
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/spike_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/idempotency_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/soak_test.js

# Cleanup
docker-compose -f docker-compose.load.yml down -v
```

## Test Overview

| Test | Duration | Max Users | Purpose |
|------|----------|-----------|---------|
| Sustained Load | 23 min | 100 | Baseline metrics |
| Spike | 6.5 min | 200 | Surge resilience |
| Idempotency | 7 min | 20 | Cache performance |
| Soak | 40 min | 30 | Stability/leaks |

## Monitoring Commands

```bash
# Real-time resource monitoring
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Database connections
docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"

# Redis memory
docker exec synapse-load-redis redis-cli INFO memory

# App logs
docker logs -f synapse-load-app
```

## Expected Results

### Sustained Load Test
- ✅ p95 < 500ms
- ✅ p99 < 1000ms
- ✅ Error rate < 5%

### Spike Test
- ✅ p95 < 1000ms
- ✅ Error rate < 10%

### Soak Test
- ✅ p95 < 500ms (stable)
- ✅ Error rate < 2%
- ✅ No memory growth

### Idempotency Test
- ✅ p95 < 300ms
- ✅ Cache hit rate > 30%

## After Running Tests

1. Document results in `docs/load-test-results.md`
2. Identify bottlenecks (DB pool, CPU, memory)
3. Optimize configuration
4. Re-run tests to validate improvements

## Troubleshooting

**Services won't start:**
```bash
docker-compose -f docker-compose.load.yml logs app
```

**High error rates:**
- Check DB connection pool size
- Monitor CPU/memory limits
- Review app logs for errors

**Slow responses:**
- Check database query performance
- Monitor Redis latency
- Review connection pool utilization
