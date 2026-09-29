# Load Test Results

This document covers the load testing infrastructure for Synapse Core and how to run the three k6 test scenarios.

As of August 28, 2026, spike and soak results should be recorded beside callback and mixed-load runs so regression review is not limited to the fastest scenario only.

## Prerequisites

- Docker and Docker Compose installed
- The `load-test` profile services must be healthy before running scripts

## Test Scripts

| Script | Purpose | Target throughput | Duration |
|---|---|---|---|
| `tests/load/callback_load.js` | Sustained callback ingestion | 1000 req/s constant | 5 minutes |
| `tests/load/search_load.js` | Concurrent search queries | 50 VUs ramping | ~5 minutes |
| `tests/load/mixed_load.js` | Realistic mix (60% callbacks, 25% reads, 15% searches) | 100→500 req/s ramping | ~5.5 minutes |

## Success Criteria (all scripts)

- p95 latency **< 200ms**
- Error rate **< 0.1%**

## Running Load Tests

### Start infrastructure

```bash
docker compose -f docker-compose.load.yml up -d postgres redis app
# Wait for the app to be healthy
sleep 15
```

### Run individual scripts

```bash
# Callback ingestion (1000 req/s for 5 minutes)
docker compose -f docker-compose.load.yml --profile load-test run --rm k6 \
  run /scripts/callback_load.js

# Search queries (50 concurrent VUs)
docker compose -f docker-compose.load.yml --profile load-test run --rm k6 \
  run /scripts/search_load.js

# Mixed realistic traffic
docker compose -f docker-compose.load.yml --profile load-test run --rm k6 \
  run /scripts/mixed_load.js
```

### Run all scripts sequentially

```bash
for script in callback_load search_load mixed_load; do
  docker compose -f docker-compose.load.yml --profile load-test run --rm k6 \
    run /scripts/${script}.js
done
```

### Override base URL or API key

```bash
docker compose -f docker-compose.load.yml --profile load-test run --rm \
  -e BASE_URL=http://app:3000 \
  -e API_KEY=my-tenant-key \
  k6 run /scripts/callback_load.js
```

## HTML Reports

Each script writes an HTML summary to `docs/` (mounted as `/results` inside the k6 container):

- `docs/callback_load_summary.html`
- `docs/search_load_summary.html`
- `docs/mixed_load_summary.html`

Open any of these in a browser after the run to review pass/fail against thresholds.

## Monitoring During Tests

```bash
# Container resource usage
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# PostgreSQL active connections
docker exec synapse-load-postgres psql -U synapse \
  -c "SELECT count(*) FROM pg_stat_activity;"

# Redis memory
docker exec synapse-load-redis redis-cli INFO memory

# App logs
docker logs -f synapse-load-app
```

## Teardown

```bash
docker compose -f docker-compose.load.yml down -v
```

## Test Environment

- **Database**: PostgreSQL 14 (max_connections=200, shared_buffers=256MB)
- **Redis**: 7-alpine (maxmemory=512mb, persistence disabled)
- **App**: Rust/Axum (2 CPU cores, 1GB RAM limit)

## Results

> Fill in after running tests against your environment.

```
callback_load:
  http_req_duration: avg=XXXms p(95)=XXXms p(99)=XXXms
  http_req_failed:   X.XX%
  http_reqs:         XXXXX

search_load:
  http_req_duration: avg=XXXms p(95)=XXXms p(99)=XXXms
  http_req_failed:   X.XX%
  http_reqs:         XXXXX

mixed_load:
  http_req_duration: avg=XXXms p(95)=XXXms p(99)=XXXms
  http_req_failed:   X.XX%
  http_reqs:         XXXXX
```

## Per-Release Reliability Scorecard

Load-test thresholds gate CI before merge, but they cannot catch regressions that only appear under real production traffic. After each production release, generate a **per-release reliability scorecard** comparing a window *before* the release against an equivalent window *after* it, so a regression can be attributed to the specific release that introduced it.

### Metrics compared

- Error rate (failed requests / total requests)
- Latency percentiles: p50, p95, p99
- Incident / alert count in each window

### Windows and overlap handling

Each release defines a `before` window (ending at the release timestamp) and an `after` window (starting at the release timestamp), both of equal length `W` (default 24h).

When releases happen close together, release B's `before` window can overlap release A's `after` window. Rather than silently producing a misleading comparison, the scorecard must detect the overlap and handle it explicitly:

- If `release_B.timestamp - release_A.timestamp < W`, the windows overlap.
- In that case, truncate the overlapping windows to the non-overlapping segment (i.e. `before_B` starts at `release_A.timestamp`) and mark the report as `overlap_adjusted: true` with the effective window length recorded.
- If the non-overlapping segment is shorter than a minimum sample threshold, the report is emitted with `insufficient_data: true` and no regression verdict, instead of a false positive.

### Regression flagging

A change is flagged as a **statistically meaningful regression** (distinct from normal noise) only when both hold:

1. The relative change exceeds a minimum effect size (default: error rate +25%, latency +20%).
2. The change is statistically significant — the before/after distributions differ beyond expected noise (e.g. a two-proportion z-test for error rate and a percentile bootstrap / Mann-Whitney U test for latency, at `p < 0.05`).

Changes that exceed the effect-size threshold but are not statistically significant are reported as `noise`, not as regressions.

### Triggering

Report generation runs as a **post-release CI/CD step**, immediately after the production deploy completes, using the same trend-analysis approach as the capacity forecasting tool. It is reporting-only: it does **not** roll back the release (automated rollback is tracked separately in issue 40 as a potential future trigger source).

### Scorecard output

```
release:            v1.2.3
released_at:        2026-08-28T14:00:00Z
window:             24h (before) / 24h (after)
overlap_adjusted:   false
insufficient_data:  false

metric        before      after       change    verdict
error_rate    0.08%       0.31%       +287%     REGRESSION
p50_latency   42ms        45ms        +7%       noise
p95_latency   180ms       260ms       +44%      REGRESSION
p99_latency   410ms       430ms       +5%       noise
incidents     0           2           +2        REGRESSION
```

### Testing

Comparison and regression-flagging logic is covered by tests against synthetic before/after metric data containing both known regressions and known non-regressions (noise), including the overlapping-window case.

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
