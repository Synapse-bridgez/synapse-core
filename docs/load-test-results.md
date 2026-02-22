# Load Test Results

This document contains baseline performance metrics and bottleneck analysis for the Synapse Core API.

## Test Environment

- **Date**: [To be filled after running tests]
- **Hardware**: [Document your test environment]
  - CPU: [cores/model]
  - RAM: [amount]
  - Disk: [type/speed]
- **Configuration**:
  - Database: PostgreSQL 14 (max_connections=200, shared_buffers=256MB)
  - Redis: 7-alpine (maxmemory=512mb)
  - App: Rust/Axum (2 CPU cores, 1GB RAM limit)

## Test Scenarios

### 1. Sustained Load Test (`callback_load.js`)

**Purpose**: Measure performance under gradually increasing realistic traffic.

**Profile**:
- Ramp up: 0 → 10 → 50 → 100 users over 11 minutes
- Sustained: 5 minutes at each level
- Total duration: ~23 minutes

**Expected Metrics**:
```
Target Thresholds:
- p95 latency: < 500ms
- p99 latency: < 1000ms
- Error rate: < 5%
```

**Results**: [Run test and fill in]
```
http_req_duration.............: avg=XXXms min=XXXms med=XXXms max=XXXms p(95)=XXXms p(99)=XXXms
http_req_failed...............: XX.XX%
http_reqs.....................: XXXXX
errors........................: XX.XX%
webhook_duration..............: avg=XXXms
iterations....................: XXXXX
vus...........................: XXX
```

**Command**:
```bash
docker-compose -f docker-compose.load.yml up -d app
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
```

---

### 2. Spike Test (`spike_test.js`)

**Purpose**: Test system behavior under sudden traffic surge (e.g., batch webhook delivery).

**Profile**:
- Normal: 10 users for 1 minute
- Spike: Sudden jump to 200 users for 3.5 minutes
- Recovery: Back to 10 users
- Total duration: ~6.5 minutes

**Expected Metrics**:
```
Target Thresholds:
- p95 latency: < 1000ms (more lenient)
- Error rate: < 10% (acceptable during spike)
```

**Results**: [Run test and fill in]
```
http_req_duration.............: avg=XXXms p(95)=XXXms p(99)=XXXms
http_req_failed...............: XX.XX%
errors........................: XX.XX%
```

**Command**:
```bash
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/spike_test.js
```

---

### 3. Soak Test (`soak_test.js`)

**Purpose**: Identify memory leaks and stability issues over extended period.

**Profile**:
- Sustained: 30 users for 30 minutes
- Total duration: ~40 minutes

**Expected Metrics**:
```
Target Thresholds:
- p95 latency: < 500ms (should remain stable)
- p99 latency: < 1000ms
- Error rate: < 2% (very low for stability)
- Memory: Should not grow unbounded
```

**Results**: [Run test and fill in]
```
http_req_duration.............: avg=XXXms p(95)=XXXms p(99)=XXXms
http_req_failed...............: XX.XX%
total_requests................: XXXXX
Memory usage (start)...........: XXX MB
Memory usage (end).............: XXX MB
Memory growth..................: XX%
```

**Command**:
```bash
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/soak_test.js
```

---

### 4. Idempotency Test (`idempotency_test.js`)

**Purpose**: Verify idempotency middleware performance and cache hit rates.

**Profile**:
- 20 concurrent users for 7 minutes
- 50% requests use shared idempotency keys
- Tests duplicate detection and Redis caching

**Expected Metrics**:
```
Target Thresholds:
- p95 latency: < 300ms (faster due to caching)
- Idempotency hit rate: > 30%
```

**Results**: [Run test and fill in]
```
http_req_duration.............: avg=XXXms p(95)=XXXms
idempotency_hits..............: XX.XX%
Status 200....................: XXXXX
Status 429 (duplicate)........: XXXXX
```

**Command**:
```bash
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/idempotency_test.js
```

---

## Bottleneck Analysis

### Identified Bottlenecks

[Fill in after running tests]

1. **Database Connection Pool**
   - Symptom: [e.g., "Connection timeouts at >80 concurrent users"]
   - Evidence: [e.g., "p99 latency spikes from 200ms to 2000ms"]
   - Recommendation: [e.g., "Increase pool size from 10 to 20"]

2. **Redis Performance**
   - Symptom: [e.g., "Idempotency checks slow under load"]
   - Evidence: [metrics]
   - Recommendation: [solution]

3. **CPU Utilization**
   - Symptom: [e.g., "CPU at 100% during spike test"]
   - Evidence: [metrics]
   - Recommendation: [e.g., "Scale horizontally or increase CPU allocation"]

4. **Memory Usage**
   - Symptom: [e.g., "Memory grows 15% during soak test"]
   - Evidence: [metrics]
   - Recommendation: [e.g., "Investigate potential leak in connection handling"]

### System Limits

Based on test results:

- **Maximum sustained throughput**: XXX requests/second
- **Maximum concurrent users**: XXX (before degradation)
- **Recommended operating capacity**: XXX req/s (70% of max)
- **Breaking point**: XXX concurrent users / XXX req/s

---

## Monitoring During Tests

Use these commands to monitor system resources during load tests:

```bash
# Monitor container stats
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Monitor PostgreSQL connections
docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"

# Monitor Redis memory
docker exec synapse-load-redis redis-cli INFO memory

# Check app logs
docker logs -f synapse-load-app
```

---

## Running the Full Test Suite

```bash
# Start the infrastructure
docker-compose -f docker-compose.load.yml up -d app

# Wait for services to be healthy
sleep 10

# Run all tests sequentially
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/callback_load.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/spike_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/idempotency_test.js
docker-compose -f docker-compose.load.yml run --rm k6 run /scripts/soak_test.js

# Cleanup
docker-compose -f docker-compose.load.yml down -v
```

---

## Recommendations for Production

[Fill in after analysis]

1. **Infrastructure Sizing**
   - [e.g., "Provision 4 CPU cores and 2GB RAM per instance"]
   - [e.g., "Use connection pooling with max 50 connections"]

2. **Scaling Strategy**
   - [e.g., "Horizontal scaling recommended above 500 req/s"]
   - [e.g., "Add load balancer for >2 instances"]

3. **Database Optimization**
   - [e.g., "Add index on anchor_transaction_id column"]
   - [e.g., "Increase shared_buffers to 512MB in production"]

4. **Monitoring & Alerts**
   - [e.g., "Alert on p95 latency > 300ms"]
   - [e.g., "Alert on error rate > 1%"]
   - [e.g., "Monitor database connection pool utilization"]

---

## Next Steps

- [ ] Run baseline tests and document results
- [ ] Identify and address critical bottlenecks
- [ ] Re-run tests after optimizations
- [ ] Set up continuous load testing in CI/CD
- [ ] Establish SLOs based on test results
- [ ] Create runbooks for handling traffic spikes
