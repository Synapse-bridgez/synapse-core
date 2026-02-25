# Load Test Validation - Quick Start Guide

Get started with load test validation in 5 minutes.

## Prerequisites

- Docker and Docker Compose installed
- Rust toolchain installed
- Repository cloned and on the feature branch

## Step 1: Run a Load Test with JSON Output

```bash
# Navigate to project root
cd drips-3

# Start the load test environment
docker-compose -f docker-compose.load.yml up -d app

# Wait for services to be ready (check health)
docker-compose -f docker-compose.load.yml ps

# Create results directory
mkdir -p test_results

# Run callback load test with JSON output
docker-compose -f docker-compose.load.yml run --rm \
  -v "$(pwd)/test_results:/results" \
  k6 run --out json=/results/callback_load_results.json \
  /scripts/callback_load.js
```

This will take about 23 minutes to complete. You'll see output like:

```
     ✓ status is 200
     ✓ response has success field
     ✓ response time < 500ms

     checks.........................: 100.00% ✓ 12345 ✗ 0
     data_received..................: 1.2 MB  54 kB/s
     data_sent......................: 890 kB  39 kB/s
     http_req_duration..............: avg=123ms min=45ms med=110ms max=890ms p(95)=280ms p(99)=450ms
     http_req_failed................: 0.00%   ✓ 0    ✗ 12345
     http_reqs......................: 12345   537/s
```

## Step 2: Validate the Results

Once the test completes, validate the results:

```bash
# Run all validation tests
cargo test --test load_validation_test

# Run with detailed output
cargo test --test load_validation_test -- --nocapture
```

You'll see output like:

```
running 11 tests

=== Load Test Validation Results ===

✓ PASSED (4):
  ✓ P95 latency 280.0ms is within threshold 500.0ms
  ✓ P99 latency 450.0ms is within threshold 1000.0ms
  ✓ Error rate 0.00% is within threshold 5.00%
  ✓ Throughput 537.00 req/s meets minimum 10.00 req/s

Overall: PASS ✓

test tests::test_p95_latency_under_threshold ... ok
test tests::test_error_rate_under_threshold ... ok
...

test result: ok. 11 passed; 0 failed; 0 ignored
```

## Step 3: Clean Up

```bash
# Stop and remove containers
docker-compose -f docker-compose.load.yml down

# Optional: Remove test results
rm -rf test_results/
```

## Using the Helper Script

For convenience, use the provided helper script:

```bash
# Make it executable (first time only)
chmod +x tests/load/run_validation.sh

# Run sustained load test
./tests/load/run_validation.sh callback_load

# Run spike test
./tests/load/run_validation.sh spike_test

# Run all tests (except soak test which takes 40+ minutes)
./tests/load/run_validation.sh all
```

## Understanding the Results

### Validation Output

The validation tests check multiple metrics:

1. **P95 Latency**: 95% of requests complete within this time
   - Threshold: 500ms (sustained), 1000ms (spike)
   - Lower is better

2. **P99 Latency**: 99% of requests complete within this time
   - Threshold: 1000ms (sustained), 2000ms (spike)
   - Lower is better

3. **Error Rate**: Percentage of failed requests
   - Threshold: 5% (sustained), 10% (spike), 2% (soak)
   - Lower is better

4. **Throughput**: Requests processed per second
   - Threshold: 10 req/s (sustained), 5 req/s (spike)
   - Higher is better

### What to Do If Tests Fail

#### High Latency
```bash
# Check database performance
docker exec synapse-load-postgres psql -U synapse -c "
  SELECT query, mean_exec_time, calls 
  FROM pg_stat_statements 
  ORDER BY mean_exec_time DESC 
  LIMIT 10;"

# Check connection pool
docker logs synapse-load-app | grep "pool"
```

#### High Error Rate
```bash
# Check application logs
docker logs synapse-load-app | grep -i error

# Check database connections
docker exec synapse-load-postgres psql -U synapse -c "
  SELECT count(*) FROM pg_stat_activity;"
```

#### Low Throughput
```bash
# Check CPU usage
docker stats synapse-load-app --no-stream

# Check for bottlenecks
docker logs synapse-load-app | grep -i "slow\|timeout\|wait"
```

## Running Different Test Scenarios

### Sustained Load Test (23 minutes)
Tests gradual load increase from 10 → 50 → 100 users.

```bash
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/callback_load_results.json \
  /scripts/callback_load.js
```

### Spike Test (6.5 minutes)
Tests sudden traffic surge (10 → 200 users).

```bash
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/spike_test_results.json \
  /scripts/spike_test.js
```

### Soak Test (40 minutes)
Tests long-term stability with 30 concurrent users.

```bash
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/soak_test_results.json \
  /scripts/soak_test.js
```

### Idempotency Test (7 minutes)
Tests duplicate detection with 50% shared keys.

```bash
docker-compose -f docker-compose.load.yml run --rm k6 run \
  --out json=test_results/idempotency_test_results.json \
  /scripts/idempotency_test.js
```

## Monitoring During Tests

Open a new terminal and monitor system resources:

```bash
# Monitor container stats
docker stats synapse-load-app synapse-load-postgres synapse-load-redis

# Monitor database connections
watch -n 5 'docker exec synapse-load-postgres psql -U synapse -c "SELECT count(*) FROM pg_stat_activity;"'

# Monitor Redis memory
watch -n 5 'docker exec synapse-load-redis redis-cli INFO memory | grep used_memory_human'

# Follow application logs
docker logs -f synapse-load-app
```

## Customizing Thresholds

To adjust thresholds for your environment, edit `tests/load_validation_test.rs`:

```rust
impl PerformanceThresholds {
    pub fn sustained_load() -> Self {
        Self {
            p95_latency_ms: 300.0,  // Changed from 500.0
            p99_latency_ms: 800.0,   // Changed from 1000.0
            error_rate_percent: 2.0, // Changed from 5.0
            // ... other thresholds
        }
    }
}
```

Then rebuild and run tests:

```bash
cargo test --test load_validation_test
```

## Next Steps

- Read the full [VALIDATION_README.md](./VALIDATION_README.md) for detailed documentation
- Integrate validation into your CI/CD pipeline
- Set up automated nightly load tests
- Create dashboards for historical trend analysis
- Configure alerts for threshold violations

## Troubleshooting

### "No such file or directory" error
Make sure you're in the project root directory (`drips-3/`).

### "Cannot connect to Docker daemon" error
Start Docker Desktop or the Docker daemon.

### Tests take too long
Use the spike test (6.5 min) or idempotency test (7 min) for faster feedback.

### JSON file is empty
The k6 test may have failed. Check the k6 output for errors.

## Support

For issues or questions:
1. Check the [VALIDATION_README.md](./VALIDATION_README.md)
2. Review the [load test README](./README.md)
3. Check application logs: `docker logs synapse-load-app`
4. Open an issue on GitHub
