# Synapse Core — Drips Wave Issues

---

## ⚠️ Contribution Guidelines — Read Before Starting Any Issue

> **These rules apply to every issue below. Your PR will be rejected if they are not followed.**

### Branch & PR Target

- **All PRs must target the `develop` branch.** Do not open PRs against `main`.
- Create your branch from the latest `develop`: `git checkout develop && git pull && git checkout -b feat/your-feature`
- Use descriptive branch names matching the pattern: `feat/`, `fix/`, `refactor/`, `docs/`, `test/`, `ci/`

### Pre-Push Quality Checklist

Before pushing your branch to GitHub, you **must** run the following commands locally and ensure they all pass:

```bash
# 1. Format — must produce zero changes
cargo fmt --all -- --check

# 2. Lint — must produce zero warnings
cargo clippy -- -D warnings

# 3. Build — must compile cleanly
cargo build

# 4. Test — all tests must pass
cargo test
```

If any of these steps fail, **do not push**. Fix the issues first.

### Preventing Merge Conflicts

Each issue is scoped to a specific set of files listed under **Key Files**. To prevent merge conflicts across contributors:

- **Only modify the files listed** in your issue's Key Files section. If you need to touch additional files, document why in the PR description.
- **Keep your branch up to date** with `develop` — rebase before opening a PR: `git fetch origin && git rebase origin/develop`
- **Coordinate with other contributors** if your issue overlaps file scope with another issue (e.g., two issues both touching `src/config.rs` — add your config fields in a single block, don't scatter changes)
- **Do not refactor or rename** files outside your issue scope. This is the #1 source of unnecessary merge conflicts.
- **Small, focused commits** — one logical change per commit. This makes rebasing easier.

### PR Requirements

- PR title must follow: `feat: <issue title>` or `fix: <issue title>`
- PR description must include: `Closes #<issue_number>`
- All CI checks must pass before requesting review
- Include tests as specified in the issue's **Validation** section
- Update documentation if your change affects public APIs or configuration

---

<!-- ==========================================================================
     CATEGORY: CALLBACK PROCESSOR SCALABILITY
     ========================================================================== -->

## Implement Partitioned Callback Processor with Configurable Worker Pool

**Complexity: High (200 points)**

### Description

The current callback processor in `src/services/processor.rs` uses a single-loop polling model with a hardcoded `LIMIT 10` batch size and a fixed 5-second poll interval. Under high callback volume this becomes a bottleneck — a single worker cannot saturate the database or keep latency low when thousands of pending transactions queue up.

Redesign the processor to support a configurable pool of N concurrent workers, each claiming disjoint batches via `FOR UPDATE SKIP LOCKED`. Workers should be managed by a supervisor task that can scale the pool up or down based on queue depth.

### Requirements and Context

- Replace the single `run_processor` loop with a `ProcessorPool` struct that spawns N worker tasks
- Each worker must use `FOR UPDATE SKIP LOCKED` (already present) to avoid contention
- Add configuration via environment variables: `PROCESSOR_WORKERS` (default 4), `PROCESSOR_BATCH_SIZE` (default 50), `PROCESSOR_POLL_INTERVAL_MS` (default 1000)
- Implement graceful shutdown: workers must finish in-flight batches before stopping
- Add Prometheus-style metrics: `processor_batches_total`, `processor_transactions_processed`, `processor_queue_depth`

### Key Files

- `src/services/processor.rs` — current single-worker loop
- `src/services/transaction_processor.rs` — per-transaction processing logic
- `src/config.rs` — add new config fields
- `src/main.rs` — wire up the new pool

### Edge Cases

- Workers must not panic on transient DB errors; log and retry next cycle
- If queue depth exceeds a configurable high-water mark, log a warning
- Ensure partition-aware queries work correctly with `SKIP LOCKED`

### Validation

- Unit tests for `ProcessorPool` lifecycle (start, process, shutdown)
- Integration test proving two workers process disjoint transaction sets
- Load test showing >10x throughput improvement vs current single-worker


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Adaptive Batch Sizing to Transaction Processor

**Complexity: High (200 points)**

### Description

The processor currently fetches a fixed batch of 10 transactions per poll cycle (hardcoded `LIMIT 10` in `src/services/processor.rs:38`). Under varying load this is inefficient — too small during spikes, unnecessary DB round-trips during low traffic.

Implement adaptive batch sizing that adjusts the `LIMIT` based on recent queue depth and processing throughput. Use an exponential moving average of queue depth to scale batch size between a configurable min and max.

### Requirements and Context

- Track a rolling average of queue depth over the last N polls
- Scale batch size: `min(max_batch, max(min_batch, avg_queue_depth * scaling_factor))`
- Config: `PROCESSOR_MIN_BATCH` (default 10), `PROCESSOR_MAX_BATCH` (default 500), `PROCESSOR_SCALING_FACTOR` (default 0.5)
- Log batch size changes at `debug` level
- Expose current batch size via the health/metrics endpoint

### Key Files

- `src/services/processor.rs` — batch fetching logic
- `src/config.rs` — new config fields

### Validation

- Unit tests for the adaptive sizing algorithm
- Integration test demonstrating batch size increases under load and decreases during idle


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Back-Pressure Mechanism for Callback Ingestion

**Complexity: High (200 points)**

### Description

The `/callback` endpoint currently accepts all incoming callbacks without regard to downstream processing capacity. If the processor falls behind, the `transactions` table grows unboundedly which degrades query performance and risks OOM on the database.

Implement a back-pressure mechanism that monitors pending transaction queue depth and begins rejecting new callbacks with `503 Service Unavailable` when the queue exceeds a configurable threshold.

### Requirements and Context

- Add a periodic background task (every 5s) that queries `SELECT COUNT(*) FROM transactions WHERE status = 'pending'` and stores the result in an `AtomicU64` on `AppState`
- The `/callback` handler checks this counter; if it exceeds `MAX_PENDING_QUEUE` (env var, default 10000), return 503 with a `Retry-After` header
- Include the current queue depth in the `/health` response
- Emit a `callback_rejected_backpressure` metric counter

### Key Files

- `src/handlers/webhook.rs` — callback handler
- `src/lib.rs` — `AppState` struct
- `src/handlers/mod.rs` — health endpoint

### Edge Cases

- The count query must not use a `FOR UPDATE` lock
- Must handle the case where the count query itself fails (fail open — accept the callback)

### Validation

- Integration test: insert >MAX_PENDING_QUEUE pending rows, verify callback returns 503
- Verify 503 includes `Retry-After` header


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Priority Queue Support to Transaction Processing

**Complexity: High (200 points)**

### Description

All pending transactions are currently processed in FIFO order (`ORDER BY created_at ASC`). High-value transactions or those from premium tenants should be processed first.

Add a `priority` column to the `transactions` table and update the processor to fetch highest-priority transactions first.

### Requirements and Context

- New migration: `ALTER TABLE transactions ADD COLUMN priority SMALLINT NOT NULL DEFAULT 0`
- Index: `CREATE INDEX idx_transactions_priority ON transactions (status, priority DESC, created_at ASC)`
- Update processor query to `ORDER BY priority DESC, created_at ASC`
- Callback handler accepts optional `priority` field (0=normal, 1=high, 2=critical)
- Multi-tenant: allow tenant config to set a default priority

### Key Files

- `migrations/` — new migration
- `src/services/processor.rs` — query ordering
- `src/handlers/webhook.rs` — accept priority field
- `src/db/models.rs` — add field to Transaction

### Validation

- Integration test: insert transactions with different priorities, verify processing order
- Verify backward compatibility (existing transactions get priority 0)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Dead Letter Queue Auto-Retry with Exponential Backoff

**Complexity: High (200 points)**

### Description

Failed transactions are moved to the DLQ (`transaction_dlq` table) but there is no automated retry mechanism. The `requeue_dlq` method in `src/services/transaction_processor.rs` is manual-only.

Implement a background worker that automatically retries DLQ entries with exponential backoff, up to a configurable max retry count.

### Requirements and Context

- Background task polls DLQ every 60 seconds
- Retry logic: `next_retry_at = last_retry_at + (base_delay * 2^retry_count)`
- Config: `DLQ_MAX_RETRIES` (default 5), `DLQ_BASE_DELAY_SECS` (default 60)
- After max retries, set a `permanently_failed` flag
- Send a webhook event (`transaction.dlq_exhausted`) when retries exhaust
- Log each retry attempt with the error reason

### Key Files

- `src/services/transaction_processor.rs` — existing `requeue_dlq`
- `src/db/models.rs` — `TransactionDlq` struct
- `src/services/webhook_dispatcher.rs` — emit event
- `src/main.rs` — spawn background task

### Validation

- Unit test for backoff calculation
- Integration test verifying retry progression and permanent failure


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Horizontal Scaling with Redis-Based Processor Coordination

**Complexity: High (200 points)**

### Description

Currently the processor assumes a single application instance. For horizontal scaling with multiple instances, we need coordination to prevent duplicate processing and enable work-stealing.

Implement Redis-based leader election and work distribution for the processor pool.

### Requirements and Context

- Use Redis `SET NX EX` for leader election with a 30-second lease
- Only the leader runs partition maintenance, settlement jobs, and webhook dispatch
- All instances run processor workers (safe due to `SKIP LOCKED`)
- Add a `processor:heartbeat:{instance_id}` key with TTL for instance discovery
- Admin endpoint to list active instances

### Key Files

- `src/services/processor.rs` — worker loop
- `src/services/lock_manager.rs` — existing distributed lock primitives
- `src/main.rs` — startup coordination

### Validation

- Integration test with simulated multi-instance scenario
- Verify leader failover within 30 seconds


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Callback Processing Pipeline Stages with Hooks

**Complexity: High (200 points)**

### Description

Transaction processing currently jumps straight from `pending` to `completed` in `TransactionProcessor::process_transaction`. There is no extensible pipeline for validation, enrichment, or external verification steps.

Implement a staged processing pipeline with configurable pre and post hooks.

### Requirements and Context

- Define a `ProcessingStage` trait with `async fn execute(&self, tx: &Transaction) -> Result<Transaction>`
- Pipeline: Validate → Enrich → Verify → Complete
- Each stage can short-circuit to `failed` with an error reason
- Feature-flag individual stages via the `feature_flags` table
- Log stage entry/exit with timing

### Key Files

- `src/services/transaction_processor.rs` — refactor `process_transaction`
- `src/services/feature_flags.rs` — gate stages
- `src/db/models.rs` — possible new status values

### Validation

- Unit test for pipeline with mock stages
- Test that a failing stage moves the transaction to DLQ


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Batched Insert for High-Volume Callback Ingestion

**Complexity: High (200 points)**

### Description

Each callback currently triggers an individual `INSERT` statement. Under high volume (>1000 callbacks/sec), this creates excessive database round-trips and connection contention.

Implement a batched insert buffer that collects callbacks and flushes them in bulk.

### Requirements and Context

- Use a `tokio::sync::mpsc` channel to buffer incoming callbacks
- Flush when buffer reaches `BATCH_INSERT_SIZE` (default 100) or `BATCH_INSERT_TIMEOUT_MS` (default 200) elapses
- Use `INSERT INTO transactions (...) VALUES (...), (...), ...` for bulk insert
- Return individual transaction IDs to each waiting caller
- Track `callback_batch_size` and `callback_batch_latency` metrics

### Key Files

- `src/handlers/webhook.rs` — send to channel instead of direct insert
- `src/db/queries.rs` — new `insert_transactions_batch` function
- `src/lib.rs` — add channel sender to `AppState`

### Edge Cases

- If the batch insert fails, individually retry each transaction
- Channel backpressure if the flusher falls behind

### Validation

- Load test showing reduced DB connections and improved throughput
- Unit test for batch timeout flush logic


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Connection Pool Autoscaling Based on Load

**Complexity: High (200 points)**

### Description

The database connection pool is statically configured with `max_connections=5` (see `src/db/mod.rs`). This is far too low for production and should dynamically adjust based on current load.

### Requirements and Context

- Increase default `max_connections` to 20 for production
- Add config: `DB_MIN_CONNECTIONS` (default 5), `DB_MAX_CONNECTIONS` (default 50)
- Monitor pool utilization (already partially done in `pool_monitor_task`)
- When utilization exceeds 80% for 3 consecutive checks, log a critical warning
- Expose pool metrics via `/health` endpoint with connection counts

### Key Files

- `src/db/mod.rs` — pool creation
- `src/main.rs` — `pool_monitor_task`
- `src/config.rs` — new config fields

### Validation

- Test that pool is created with configured min/max
- Load test verifying pool scales under pressure


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Concurrent Webhook Delivery with Configurable Parallelism

**Complexity: High (200 points)**

### Description

The webhook dispatcher in `src/services/webhook_dispatcher.rs` processes deliveries sequentially in a single loop (`for delivery in deliveries`). This means a slow endpoint blocks all other deliveries.

Implement concurrent delivery with configurable parallelism using `futures::stream::buffer_unordered`.

### Requirements and Context

- Process deliveries concurrently with `WEBHOOK_DELIVERY_CONCURRENCY` (default 10) parallel requests
- Use `futures::stream::iter(...).buffer_unordered(concurrency)` pattern
- Add per-endpoint rate limiting to prevent flooding a single endpoint
- Track `webhook_delivery_latency_ms` histogram metric

### Key Files

- `src/services/webhook_dispatcher.rs` — `process_pending` method
- `src/config.rs` — concurrency config

### Validation

- Test that N deliveries to a slow endpoint don't block other endpoints
- Verify rate limiting works per-endpoint


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: IDEMPOTENCY IMPROVEMENTS
     ========================================================================== -->

## Add Database-Backed Idempotency Fallback When Redis Is Unavailable

**Complexity: High (200 points)**

### Description

The idempotency middleware (`src/middleware/idempotency.rs`) relies entirely on Redis. If Redis becomes unavailable, the middleware silently falls through (line 212: `Err(e) => next.run(request).await`), meaning duplicate callbacks can create duplicate transactions.

Implement a PostgreSQL-based fallback that activates when Redis is unreachable.

### Requirements and Context

- New migration: `CREATE TABLE idempotency_keys (key VARCHAR(255) PRIMARY KEY, status VARCHAR(20), response JSONB, created_at TIMESTAMPTZ DEFAULT NOW(), expires_at TIMESTAMPTZ)`
- When Redis fails, check/insert into the `idempotency_keys` table using `INSERT ... ON CONFLICT`
- Background cleanup job: delete expired keys older than 24 hours
- Log a warning when falling back to Postgres
- Track `idempotency_fallback_count` metric

### Key Files

- `src/middleware/idempotency.rs` — add fallback logic
- `migrations/` — new migration
- `src/db/queries.rs` — new query functions

### Edge Cases

- Handle race condition: two requests for the same key hit Postgres simultaneously (use `ON CONFLICT DO NOTHING` with `RETURNING`)
- Transition back to Redis seamlessly when it recovers

### Validation

- Integration test: simulate Redis down, verify idempotency still works via Postgres
- Test cleanup job removes expired keys
- Verify no duplicate transactions under concurrent requests with same key


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Idempotency Key Scoping Per Tenant

**Complexity: High (200 points)**

### Description

Idempotency keys are currently global — if two different tenants send the same idempotency key, the second request will incorrectly return a cached response from the first tenant.

Scope idempotency keys to include the tenant identifier.

### Requirements and Context

- Change cache key format from `idempotency:{key}` to `idempotency:{tenant_id}:{key}`
- Extract tenant ID from `X-Tenant-Id` header or API key lookup
- Update all Redis key operations in `IdempotencyService`
- Backward compatible: if no tenant ID is present, use `default` as the scope

### Key Files

- `src/middleware/idempotency.rs` — key format
- `src/middleware/auth.rs` — tenant extraction

### Validation

- Test: two tenants with the same idempotency key get independent responses
- Test: requests without tenant ID still work (backward compatibility)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Idempotency Response Body Caching

**Complexity: High (200 points)**

### Description

The idempotency middleware currently caches only a generic `{"status": "success"}` body (line 176) instead of the actual response body. This means cached responses don't return the correct transaction data.

Cache and replay the full original response body.

### Requirements and Context

- Capture the actual response body bytes before storing
- Reconstruct the full response (status code, body, select headers) on cache hit
- Limit cached body size to 64KB to prevent Redis memory issues
- Preserve `Content-Type` header in cached response
- Add `X-Idempotent-Replayed: true` header to cached responses

### Key Files

- `src/middleware/idempotency.rs` — `idempotency_middleware` function
- `CachedResponse` struct — extend with content_type and full body

### Edge Cases

- Response body exceeds 64KB: cache status only, log a warning
- Binary response bodies: store as base64

### Validation

- Integration test: first request returns transaction JSON, second request returns identical JSON
- Verify `X-Idempotent-Replayed` header is present on cached responses


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Idempotency Key Validation and Normalization

**Complexity: High (200 points)**

### Description

The idempotency middleware accepts any string as a key with no validation. This allows excessively long keys, keys with control characters, or empty keys that could cause Redis issues.

### Requirements and Context

- Validate key length: min 1, max 255 characters
- Allow only alphanumeric, hyphens, underscores, and dots
- Reject keys with control characters or whitespace
- Return 400 with a clear error message on invalid keys
- Trim whitespace from key values

### Key Files

- `src/middleware/idempotency.rs` — add validation after key extraction

### Validation

- Unit tests for valid/invalid key patterns
- Test that overlong keys return 400


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Idempotency Lock Timeout Recovery

**Complexity: High (200 points)**

### Description

If a request acquires an idempotency lock (line 65-72) but the process crashes before storing the response, the lock remains for 5 minutes. During this window, all requests with that key receive `429 Too Many Requests`.

Implement lock recovery: detect stale locks and allow re-processing.

### Requirements and Context

- Store the lock holder's instance ID and timestamp in the lock value
- A background task checks for locks older than 2 minutes without a corresponding cached response
- Stale locks are deleted, allowing the next request to reprocess
- Log lock recovery events at `warn` level

### Key Files

- `src/middleware/idempotency.rs` — lock value format and recovery logic

### Validation

- Test: simulate crash (set lock, don't store response), verify recovery after timeout
- Test: normal flow is not affected by recovery task


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Idempotency Metrics Dashboard Data

**Complexity: High (200 points)**

### Description

There are no metrics for the idempotency system. Operators cannot see cache hit rates, lock contention, or fallback usage.

### Requirements and Context

- Track and expose via `/cache/metrics`:
  - `idempotency_cache_hits`
  - `idempotency_cache_misses`
  - `idempotency_lock_acquired`
  - `idempotency_lock_contention` (429 responses)
  - `idempotency_errors`
- Use `AtomicU64` counters on `AppState` (same pattern as `QueryCache::metrics`)

### Key Files

- `src/middleware/idempotency.rs` — increment counters
- `src/handlers/stats.rs` — expose endpoint
- `src/lib.rs` — add counters to `AppState`

### Validation

- Unit test verifying counter increments
- Verify metrics appear in `/cache/metrics` response


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: CIRCUIT BREAKER RESILIENCE
     ========================================================================== -->

## Add Circuit Breaker to Redis Connections

**Complexity: High (200 points)**

### Description

The Stellar Horizon client has a circuit breaker (`src/stellar/client.rs`), but Redis connections — which are critical for idempotency, caching, and quotas — have no circuit breaker protection. A Redis outage causes every request to hang on connection attempts.

Implement circuit breaker protection for all Redis operations.

### Requirements and Context

- Create `RedisCircuitBreaker` wrapper around `redis::Client` using the `failsafe` crate
- Configuration: `REDIS_CB_FAILURE_THRESHOLD` (default 5), `REDIS_CB_RESET_TIMEOUT_SECS` (default 30)
- When circuit is open, all Redis operations fail immediately with a descriptive error
- Integrate with `IdempotencyService`, `QueryCache`, and `QuotaManager`
- Expose circuit state in `/health` response

### Key Files

- `src/middleware/idempotency.rs` — uses Redis
- `src/services/query_cache.rs` — uses Redis
- `src/middleware/quota.rs` — uses Redis
- `src/health.rs` — `RedisChecker`

### Validation

- Integration test: simulate Redis failures, verify circuit opens after threshold
- Test: circuit open causes immediate failure, not timeout
- Test: circuit resets after timeout


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Circuit Breaker for PostgreSQL Operations

**Complexity: High (200 points)**

### Description

PostgreSQL is the most critical dependency, but there is no circuit breaker protection. The pool manager (`src/db/pool_manager.rs`) handles failover to replicas but doesn't prevent cascading failures when the primary is overloaded.

### Requirements and Context

- Wrap critical DB operations (insert, update) with a circuit breaker
- Distinguish between connection errors (count toward breaker) and query errors (don't)
- When primary circuit opens, route writes to a local queue (WAL-style file) for replay when primary recovers
- Config: `DB_CB_FAILURE_THRESHOLD` (default 10), `DB_CB_RESET_TIMEOUT_SECS` (default 60)
- Alert condition: circuit open should trigger a high-severity log

### Key Files

- `src/db/pool_manager.rs` — add circuit breaker
- `src/db/queries.rs` — wrap query functions
- `src/config.rs` — new config fields

### Validation

- Test: simulate primary failure, verify circuit opens
- Test: circuit recovery and queue replay
- Verify read queries still work via replica when primary circuit is open


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Half-Open State Monitoring to Horizon Circuit Breaker

**Complexity: High (200 points)**

### Description

The existing Horizon circuit breaker (`src/stellar/client.rs`) uses the `failsafe` crate which doesn't expose the half-open state clearly. Operators cannot tell whether the breaker is testing recovery or fully open.

### Requirements and Context

- Extend `circuit_state()` to return `"closed"`, `"open"`, or `"half-open"`
- Add `circuit_breaker_state_changes_total` metric counter
- Log state transitions at `warn` level
- Include last failure reason and timestamp in state response
- Add a `/health/horizon` sub-endpoint with detailed circuit breaker state

### Key Files

- `src/stellar/client.rs` — `circuit_state` method
- `src/health.rs` — `HorizonChecker`

### Validation

- Test: force circuit open, verify state = "open"
- Test: after reset timeout, verify state transitions to "half-open"
- Test: successful probe in half-open closes the circuit


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Circuit Breaker State Persistence Across Restarts

**Complexity: High (200 points)**

### Description

Circuit breaker state is in-memory only. On restart, a circuit breaker that was open (protecting against a known failure) resets to closed, potentially causing a thundering herd against a still-failing service.

### Requirements and Context

- Persist circuit breaker states to Redis with a TTL matching the reset timeout
- On startup, read persisted states and initialize breakers accordingly
- Keys: `cb:state:{service_name}` with JSON value `{state, opened_at, failure_count, last_error}`
- Only persist `open` states (closed is the default)

### Key Files

- `src/stellar/client.rs` — Horizon breaker
- New: `src/services/circuit_breaker.rs` — shared circuit breaker infrastructure

### Validation

- Test: open circuit, restart app, verify circuit starts in open state
- Test: TTL expiry causes circuit to start closed after long downtime


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Circuit Breaker to Webhook Delivery HTTP Client

**Complexity: High (200 points)**

### Description

The webhook dispatcher (`src/services/webhook_dispatcher.rs`) retries failed deliveries but has no circuit breaker per endpoint. If an endpoint is permanently down, the dispatcher wastes resources attempting delivery every 30 seconds forever.

### Requirements and Context

- Per-endpoint circuit breaker: after 5 consecutive failures, stop attempting delivery for that endpoint
- Automatically re-enable after 10 minutes
- When an endpoint's circuit is open, skip its deliveries in `process_pending`
- Admin API endpoint to manually reset a circuit breaker for an endpoint
- Store circuit state in the `webhook_endpoints` table: `circuit_state VARCHAR(20)`, `circuit_opened_at TIMESTAMPTZ`

### Key Files

- `src/services/webhook_dispatcher.rs` — delivery logic
- `migrations/` — add columns to `webhook_endpoints`

### Validation

- Test: 5 failures opens circuit for an endpoint
- Test: other endpoints are unaffected
- Test: circuit resets after timeout


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Cascading Circuit Breaker Dashboard Endpoint

**Complexity: High (200 points)**

### Description

There is no unified view of circuit breaker states across all services (Horizon, Redis, Postgres, webhook endpoints).

### Requirements and Context

- New endpoint: `GET /admin/circuit-breakers` (admin-auth protected)
- Returns JSON with all circuit breaker states:
  ```json
  {
    "horizon": { "state": "closed", "failure_count": 0 },
    "redis": { "state": "open", "opened_at": "...", "last_error": "..." },
    "postgres": { "state": "closed" },
    "webhook_endpoints": [...]
  }
  ```
- Include time-in-current-state for each breaker

### Key Files

- New handler in `src/handlers/admin/`
- Wire into admin routes in `src/main.rs`

### Validation

- Test: endpoint returns correct states for all breakers
- Verify admin auth is required


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: CI/CD RELIABILITY
     ========================================================================== -->

## Add Redis Service Container to CI Workflow

**Complexity: High (200 points)**

### Description

The CI workflow (`.github/workflows/rust.yml`) only runs PostgreSQL as a service container. Many tests require Redis (idempotency, caching, quota), so they either skip or fail.

### Requirements and Context

- Add Redis 7 Alpine service container alongside PostgreSQL
- Set `REDIS_URL=redis://localhost:6379` environment variable for all steps
- Ensure Redis healthcheck passes before tests run

### Key Files

- `.github/workflows/rust.yml`

### Validation

- CI pipeline passes with Redis-dependent tests enabled
- Verify Redis service starts and passes healthcheck


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Cargo Clippy and Rustfmt Checks to CI Pipeline

**Complexity: High (200 points)**

### Description

The CI pipeline only runs `cargo build` and `cargo test`. There is no automated linting or formatting check, meaning PRs with style violations can merge.

### Requirements and Context

- Add step: `cargo fmt --all -- --check` (fail if not formatted)
- Add step: `cargo clippy -- -D warnings` (fail on clippy warnings)
- Run these before `cargo build` to fail fast
- Cache clippy and fmt artifacts with `actions/cache`

### Key Files

- `.github/workflows/rust.yml`

### Validation

- CI fails on a PR with formatting issues
- CI fails on a PR with clippy warnings


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Cargo Build Caching in CI

**Complexity: High (200 points)**

### Description

Each CI run compiles all dependencies from scratch. For a project with 60+ dependencies, this adds 5-10 minutes to every pipeline.

### Requirements and Context

- Use `actions/cache` to cache `~/.cargo/registry`, `~/.cargo/git`, and `target/`
- Cache key based on `Cargo.lock` hash
- Restore cache before build step
- Add `sccache` as an optional acceleration layer

### Key Files

- `.github/workflows/rust.yml`

### Validation

- Second CI run completes significantly faster than first
- Cache invalidates when `Cargo.lock` changes


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Separate CI Jobs for Unit Tests vs Integration Tests

**Complexity: High (200 points)**

### Description

All tests run in a single `cargo test` step. Integration tests (marked `#[ignore]`) are skipped, and there is no way to run them separately. This means integration tests never run in CI.

### Requirements and Context

- Split into two CI jobs: `unit-tests` and `integration-tests`
- Unit tests: `cargo test --lib --bins` (no external services needed)
- Integration tests: `cargo test -- --ignored` (requires Postgres + Redis)
- Integration tests run in parallel with unit tests
- Both must pass for the pipeline to succeed

### Key Files

- `.github/workflows/rust.yml` — restructure into multiple jobs

### Validation

- Both jobs appear in CI dashboard
- Integration tests actually execute and pass


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Security Audit Step to CI Pipeline

**Complexity: High (200 points)**

### Description

There is no automated security scanning for known vulnerabilities in dependencies.

### Requirements and Context

- Add `cargo install cargo-audit` and `cargo audit` step
- Run after build, before tests
- Fail the pipeline on known vulnerabilities with severity >= moderate
- Allow maintainers to add exceptions in `deny.toml` for false positives

### Key Files

- `.github/workflows/rust.yml`
- New: `deny.toml` (audit configuration)

### Validation

- CI catches a known vulnerable dependency (test with an older version)
- Exceptions in `deny.toml` are respected


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement CI Matrix Testing for Multiple Rust Versions

**Complexity: High (200 points)**

### Description

CI only tests against the default Rust version on `ubuntu-latest`. We should verify compatibility with both stable and MSRV.

### Requirements and Context

- Add a matrix strategy: `["stable", "1.75.0"]` (MSRV should match Cargo.toml edition)
- Use `dtolnay/rust-toolchain` action for reliable Rust installation
- Only run clippy on stable (MSRV may have false positives)

### Key Files

- `.github/workflows/rust.yml`

### Validation

- CI shows two parallel jobs for each Rust version
- Both pass on main branch


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Docker Image Build and Push to CI

**Complexity: High (200 points)**

### Description

The project has a `dockerfile` but there is no CI step to build and push the Docker image. Deployments are manual.

### Requirements and Context

- Build Docker image on every merge to `main`
- Tag with: `latest`, git SHA, and semver from `Cargo.toml`
- Push to GitHub Container Registry (ghcr.io)
- Only push on `main` branch, not on PRs (PRs only build to verify it works)
- Multi-stage build should cache builder layer

### Key Files

- `dockerfile` — existing Dockerfile
- `.github/workflows/rust.yml` — add build step

### Validation

- Docker image builds successfully in CI
- Image is pushed to ghcr.io on main branch merges


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Migration Validation Step to CI

**Complexity: High (200 points)**

### Description

Migrations run in CI (`sqlx migrate run`) but there is no validation that migrations are reversible or that the schema is consistent.

### Requirements and Context

- After running migrations up, run migrations down: `sqlx migrate revert --all`
- Then run them up again to verify idempotency
- Verify all `.down.sql` files exist for every `.sql` migration
- Add a check that migration filenames follow the naming convention

### Key Files

- `.github/workflows/rust.yml`
- `migrations/` — all migration files

### Validation

- CI fails if a down migration is missing
- CI fails if running up→down→up produces errors


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement PR Label-Based Test Scope in CI

**Complexity: High (200 points)**

### Description

Running the full test suite on every PR is slow. Allow PR labels to control which tests run.

### Requirements and Context

- Label `skip-integration-tests`: skip integration tests
- Label `full-suite`: run all tests including load tests
- Label `docs-only`: skip all tests, only check formatting
- Default (no label): run unit tests + integration tests

### Key Files

- `.github/workflows/rust.yml` — conditional job execution

### Validation

- PR with `docs-only` label skips tests
- PR with no labels runs default suite


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Test Coverage Reporting to CI

**Complexity: High (200 points)**

### Description

There is no test coverage tracking. Contributors cannot see which code paths are untested.

### Requirements and Context

- Use `cargo-tarpaulin` or `cargo-llvm-cov` for coverage
- Generate coverage report and upload to Codecov or Coveralls
- Add coverage badge to README
- Set minimum coverage threshold: warn below 60%, fail below 40%
- Exclude test files and generated code from coverage metrics

### Key Files

- `.github/workflows/rust.yml` — add coverage step
- `README.md` — add badge

### Validation

- Coverage report appears in CI artifacts
- PR comments show coverage diff


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: DATABASE & QUERY OPTIMIZATION
     ========================================================================== -->

## Implement Read-Replica Routing for Query Endpoints

**Complexity: High (200 points)**

### Description

The `PoolManager` (`src/db/pool_manager.rs`) supports a replica URL but it is not utilized by any query endpoints. All reads hit the primary database.

Route read-only queries (list transactions, search, stats, settlements) to the replica when available.

### Requirements and Context

- Add a `read_pool()` method to `PoolManager` that returns the replica pool (or primary if no replica)
- Update all read-only handlers to use `state.pool_manager.read_pool()`
- Write operations must always use the primary
- Handle replica lag: add `X-Read-Consistency: eventual` header to replica-served responses
- Log when a query is routed to the replica

### Key Files

- `src/db/pool_manager.rs` — add `read_pool()` method
- `src/handlers/webhook.rs` — `get_transaction`, `list_transactions`
- `src/handlers/search.rs` — already uses pool_manager
- `src/handlers/stats.rs` — read-only queries
- `src/handlers/settlements.rs` — list/get settlements

### Validation

- Test: with replica configured, read queries go to replica
- Test: write queries always go to primary
- Test: without replica, reads fall back to primary


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Query Timeout Configuration for Database Operations

**Complexity: High (200 points)**

### Description

There are no query timeouts configured. A slow query can hold a connection indefinitely, eventually exhausting the pool.

### Requirements and Context

- Set `statement_timeout` on the connection pool: `DB_STATEMENT_TIMEOUT_MS` (default 30000)
- Set `idle_timeout` on pool connections: `DB_IDLE_TIMEOUT_SECS` (default 600)
- Long-running queries (exports, reconciliation) should use a dedicated pool with a longer timeout
- Log queries that are cancelled due to timeout

### Key Files

- `src/db/mod.rs` — pool options
- `src/config.rs` — new config fields

### Validation

- Test: a query exceeding the timeout is cancelled
- Test: export endpoint uses longer timeout


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Optimize Transaction Search with Materialized Indexes

**Complexity: High (200 points)**

### Description

The search function in `src/db/queries.rs` builds dynamic SQL queries without leveraging composite indexes. For multi-filter searches, this results in sequential scans.

### Requirements and Context

- Add composite index: `(status, asset_code, created_at DESC)`
- Add partial index: `WHERE status = 'pending'` for processor queries
- Add GIN index on `metadata` column for JSON path queries
- Run `EXPLAIN ANALYZE` on common query patterns and document results
- Update `search_transactions` to hint the planner with index-aligned ordering

### Key Files

- `migrations/` — new migration with indexes
- `src/db/queries.rs` — optimize query construction
- `docs/` — document query plans

### Validation

- `EXPLAIN ANALYZE` shows index scan instead of seq scan for common patterns
- Search performance improves by >50% on tables with >100K rows


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Database Connection Pool Warm-Up on Startup

**Complexity: High (200 points)**

### Description

The connection pool starts with zero connections and lazily creates them on first use. The first few requests experience elevated latency while connections are established.

### Requirements and Context

- Set `min_connections` on the pool to `DB_MIN_CONNECTIONS` (default 5)
- After pool creation, execute a simple `SELECT 1` on each min connection to establish them eagerly
- Log pool warm-up completion time
- Delay readiness probe (`/ready`) until warm-up completes

### Key Files

- `src/db/mod.rs` — pool options
- `src/readiness.rs` — delay readiness

### Validation

- Test: on startup, pool has `min_connections` established before accepting traffic
- Verify first request latency is similar to steady-state


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Partition Pruning Optimization for Date-Range Queries

**Complexity: High (200 points)**

### Description

Date-range queries (daily totals, exports) may scan all partitions instead of only relevant ones. PostgreSQL's partition pruning needs explicit date bounds in the WHERE clause.

### Requirements and Context

- Ensure all date-range queries include explicit `created_at >= $start AND created_at < $end` bounds
- Update `get_daily_totals` to use exact date bounds instead of `NOW() - INTERVAL`
- Add `EXPLAIN ANALYZE` logging for partition-pruning verification in development mode
- Document partition pruning behavior in `docs/partition_architecture.md`

### Key Files

- `src/db/queries.rs` — date-range queries
- `docs/partition_architecture.md` — documentation

### Validation

- `EXPLAIN` output shows only relevant partitions are scanned
- No full table scans for date-bounded queries


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Async-Safe Database Query Timeout with Cancellation

**Complexity: High (200 points)**

### Description

Currently if a database query hangs, the Tokio task waits indefinitely. Implement proper async timeout with query cancellation.

### Requirements and Context

- Wrap all database queries with `tokio::time::timeout`
- On timeout, the underlying connection should be reset (not just abandoned)
- Different timeout tiers: read queries (5s), write queries (10s), admin queries (60s)
- Track `db_query_timeout_total` metric
- Log timed-out queries with their SQL (sanitized — no parameter values)

### Key Files

- `src/db/queries.rs` — wrap queries
- `src/config.rs` — timeout configuration

### Validation

- Test: slow query triggers timeout and returns error
- Verify connection is returned to pool after timeout (not leaked)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: WEBHOOK SYSTEM ENHANCEMENTS
     ========================================================================== -->

## Implement Webhook Endpoint Health Scoring

**Complexity: High (200 points)**

### Description

There is no tracking of webhook endpoint reliability. Endpoints that consistently fail should be flagged or auto-disabled rather than consuming retry resources.

### Requirements and Context

- Track success rate per endpoint over a rolling 24-hour window
- New columns on `webhook_endpoints`: `success_rate NUMERIC`, `total_deliveries INT`, `last_success_at TIMESTAMPTZ`
- Auto-disable endpoints with <10% success rate over last 100 deliveries
- Admin API to view endpoint health scores
- Notification when an endpoint is auto-disabled

### Key Files

- `src/services/webhook_dispatcher.rs` — update statistics after each delivery
- `migrations/` — add columns
- `src/handlers/admin/` — health score endpoint

### Validation

- Test: endpoint with >90% failure rate is auto-disabled
- Test: admin can re-enable a disabled endpoint


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Webhook Payload Signing Version Support

**Complexity: High (200 points)**

### Description

The current webhook signing (`sign_payload` in `src/services/webhook_dispatcher.rs`) uses HMAC-SHA256 only. To support future signing algorithm upgrades, implement versioned signatures.

### Requirements and Context

- Include signing version in header: `X-Webhook-Signature: v1=sha256_hex`
- Support `v1` (HMAC-SHA256, current) and prepare `v2` (HMAC-SHA512) structure
- Include timestamp in signed payload to prevent replay attacks: `{timestamp}.{body}`
- Add `X-Webhook-Timestamp` header
- Consumers can verify: `HMAC(secret, timestamp + "." + body) == signature`

### Key Files

- `src/services/webhook_dispatcher.rs` — `sign_payload` function and delivery headers

### Validation

- Test: v1 signature matches expected value
- Test: timestamp is included in signed content
- Document verification algorithm in `docs/webhook-authentication.md`


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Webhook Delivery Rate Limiting Per Endpoint

**Complexity: High (200 points)**

### Description

The webhook dispatcher can overwhelm slow endpoints by sending all pending deliveries at once.

### Requirements and Context

- Add `max_delivery_rate` column to `webhook_endpoints` (default 10 per minute)
- Use Redis-based token bucket per endpoint
- If rate limit is reached, delay delivery to next cycle
- Admin API to adjust rate limit per endpoint

### Key Files

- `src/services/webhook_dispatcher.rs` — rate check before delivery
- `migrations/` — new column

### Validation

- Test: endpoint with rate limit 5/min receives only 5 deliveries per minute
- Test: delayed deliveries are processed in next cycle


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Webhook Event Filtering by Transaction Properties

**Complexity: High (200 points)**

### Description

Webhook endpoints subscribe to event types (`event_types` column) but cannot filter by transaction properties (asset code, amount threshold, tenant). This means endpoints receive events they don't care about.

### Requirements and Context

- Add `filter_rules JSONB` column to `webhook_endpoints`
- Filter rules example: `{"asset_codes": ["USD", "EUR"], "min_amount": "100.00"}`
- Apply filters in `endpoints_for_event` before enqueuing deliveries
- Document filter rule syntax in webhook docs

### Key Files

- `src/services/webhook_dispatcher.rs` — `endpoints_for_event` query
- `migrations/` — add column

### Validation

- Test: endpoint with `asset_codes: ["USD"]` only receives USD events
- Test: endpoint without filters receives all events


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Webhook Delivery Deduplication

**Complexity: High (200 points)**

### Description

If `enqueue` is called twice for the same transaction and event type (e.g., due to a retry), duplicate deliveries are created.

### Requirements and Context

- Add unique constraint: `UNIQUE (endpoint_id, transaction_id, event_type)`
- Use `INSERT ... ON CONFLICT DO NOTHING` in `enqueue`
- Log skipped duplicates at `debug` level

### Key Files

- `src/services/webhook_dispatcher.rs` — `enqueue` method
- `migrations/` — unique constraint

### Validation

- Test: calling `enqueue` twice for the same event creates only one delivery
- Existing deliveries are unaffected by migration


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: OBSERVABILITY & MONITORING
     ========================================================================== -->

## Implement Structured Request/Response Logging with Correlation IDs

**Complexity: High (200 points)**

### Description

The request logger (`src/middleware/request_logger.rs`) is basic. Production debugging requires structured logging with correlation IDs that trace a request through all layers.

### Requirements and Context

- Generate a UUID correlation ID for each request (or use `X-Request-Id` header if present)
- Include `correlation_id` in all log lines within the request scope
- Log: method, path, status code, duration, body size, client IP
- Attach correlation ID to the response as `X-Request-Id` header
- Include correlation ID in error responses

### Key Files

- `src/middleware/request_logger.rs` — enhance middleware
- `src/error.rs` — include correlation ID in error responses

### Validation

- Test: every request gets a correlation ID
- Test: all logs for a request share the same correlation ID
- Test: response includes `X-Request-Id` header


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add OpenTelemetry Metrics Exporter Alongside Tracing

**Complexity: High (200 points)**

### Description

The telemetry module (`src/telemetry.rs`) only exports traces via OTLP. There are no metrics exports — all metrics are in-memory `AtomicU64` counters.

### Requirements and Context

- Add OpenTelemetry metrics SDK alongside the trace SDK
- Export metrics via OTLP to the same endpoint as traces
- Convert existing metrics (cache hits/misses, pool usage) to OTel counters/gauges
- Add histograms: request duration, query duration, webhook delivery duration
- Add gauge: active connections, pending queue depth

### Key Files

- `src/telemetry.rs` — add metrics provider
- `src/metrics.rs` — restructure to use OTel
- `Cargo.toml` — add `opentelemetry` metrics feature

### Validation

- Metrics appear in a local OTLP collector (Grafana Alloy or similar)
- Dashboard shows request rate, latency percentiles, error rate


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Prometheus-Compatible Metrics Endpoint

**Complexity: High (200 points)**

### Description

The current metrics implementation (`src/metrics.rs`) is minimal and not Prometheus-compatible. Most monitoring stacks expect a `/metrics` endpoint in Prometheus text format.

### Requirements and Context

- Add `/metrics` endpoint that returns Prometheus text format
- Include: request count/duration histograms by path, DB pool stats, cache metrics, processor queue depth
- Use the `metrics` or `prometheus` crate
- Protect the endpoint with admin auth or a separate metrics port

### Key Files

- `src/metrics.rs` — restructure
- `src/handlers/mod.rs` — add route

### Validation

- Prometheus can scrape the `/metrics` endpoint
- All key metrics appear in the output


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Health Check Endpoint Severity Levels

**Complexity: High (200 points)**

### Description

The health endpoint returns `healthy`, `degraded`, or `unhealthy` but doesn't distinguish between soft failures (cache miss) and hard failures (DB down).

### Requirements and Context

- Each dependency check should report severity: `critical` or `non-critical`
- Postgres is `critical` — if unhealthy, overall = `unhealthy`
- Redis is `non-critical` — if unhealthy, overall = `degraded`
- Horizon is `non-critical` — if unhealthy, overall = `degraded`
- Response includes dependency severity in the JSON output

### Key Files

- `src/health.rs` — already has `determine_overall_status` with `critical_deps`

### Validation

- Test: Postgres down = unhealthy, Redis down = degraded
- Verify existing tests still pass


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Slow Query Detection and Alerting

**Complexity: High (200 points)**

### Description

There is no visibility into slow database queries. Production performance issues are discovered only when users report latency.

### Requirements and Context

- Wrap database queries with timing instrumentation
- Log queries exceeding `SLOW_QUERY_THRESHOLD_MS` (default 500) at `warn` level
- Include query text (parameterized), duration, and rows returned
- Track `db_slow_query_total` counter metric
- In development mode, log all query timings at `debug` level

### Key Files

- `src/db/queries.rs` — add timing around queries
- `src/config.rs` — threshold config

### Validation

- Test: inject artificial delay, verify slow query is logged
- Verify parameterized SQL is logged (not raw values)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Startup Readiness Probe That Validates All Dependencies

**Complexity: High (200 points)**

### Description

The readiness state (`src/readiness.rs`) initially returns `ready=true` immediately, even before migrations run or services initialize. Kubernetes would route traffic to an unready instance.

### Requirements and Context

- Start with `ready=false`
- Check: migrations complete
- Check: Redis connection works (PING)
- Check: Horizon reachable
- Set `ready=true` only after all checks pass
- Log each check result during startup

### Key Files

- `src/readiness.rs` — change initial state
- `src/main.rs` — call readiness checks after initialization

### Validation

- Test: `/ready` returns 503 before initialization completes
- Test: `/ready` returns 200 after initialization


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: SECURITY HARDENING
     ========================================================================== -->

## Implement API Key Authentication for Callback Endpoints

**Complexity: High (200 points)**

### Description

The `/callback` and `/webhook` endpoints are only protected by HMAC signature verification for webhooks, but the callback endpoint has no authentication at all. Anyone who discovers the URL can inject fake transactions.

### Requirements and Context

- Require `X-API-Key` header on all callback endpoints
- API keys stored in the `tenants` table (already has `webhook_secret`)
- Rate limit per API key (integration with existing `QuotaManager`)
- Return 401 on missing/invalid key
- Log authentication failures with source IP

### Key Files

- `src/middleware/auth.rs` — add API key validation
- `src/handlers/webhook.rs` — apply middleware
- `src/db/queries.rs` — API key lookup query

### Validation

- Test: request without API key returns 401
- Test: request with invalid API key returns 401
- Test: request with valid API key passes through


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Rate Limiting Per Tenant with Configurable Limits

**Complexity: High (200 points)**

### Description

The `QuotaManager` (`src/middleware/quota.rs`) exists but is not integrated into the request flow. Rate limiting is commented out in `main.rs` (lines 206-212).

### Requirements and Context

- Wire `QuotaManager` middleware into the callback and webhook routes
- Per-tenant limits stored in `tenants.rate_limit_per_minute` column
- Default limit for unauthenticated requests: 100 req/min
- Return 429 with `Retry-After` header and quota status headers:
  - `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`
- Skip rate limiting for admin routes

### Key Files

- `src/middleware/quota.rs` — existing implementation
- `src/main.rs` — uncomment and wire up
- `src/handlers/mod.rs` — route configuration

### Validation

- Test: exceeding rate limit returns 429 with correct headers
- Test: different tenants have independent rate limits


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Request Body Size Limits

**Complexity: High (200 points)**

### Description

There are no request body size limits. An attacker could send a massive payload to exhaust memory.

### Requirements and Context

- Use `tower_http::limit::RequestBodyLimitLayer`
- Default limit: 1MB for callback/webhook endpoints
- Higher limit: 10MB for admin endpoints (backups, exports)
- Return 413 Payload Too Large with a clear error message

### Key Files

- `src/main.rs` or `src/lib.rs` — add limit layer
- `Cargo.toml` — may need `tower-http` `limit` feature

### Validation

- Test: payload exceeding 1MB returns 413
- Test: normal-sized payload passes through


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add CORS Configuration for GraphQL Playground

**Complexity: High (200 points)**

### Description

`tower-http` CORS is imported but not configured beyond defaults. The GraphQL playground and external API consumers need proper CORS headers.

### Requirements and Context

- Configure allowed origins via `CORS_ALLOWED_ORIGINS` env var (comma-separated)
- Default: no CORS (allow same-origin only)
- Allow credentials for admin endpoints
- Set appropriate `max_age` for preflight caching

### Key Files

- `src/main.rs` — add CORS layer
- `src/config.rs` — CORS config

### Validation

- Test: cross-origin request from allowed origin succeeds
- Test: cross-origin request from disallowed origin is rejected


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Secrets Rotation Without Downtime

**Complexity: High (200 points)**

### Description

The `SecretsManager` (`src/secrets.rs`) loads secrets once at startup from Vault. If a secret is rotated in Vault, the application must be restarted.

### Requirements and Context

- Implement periodic secret refresh (every 5 minutes)
- Use double-buffering: new secret becomes active, old secret remains valid for a grace period
- Apply to: `ANCHOR_WEBHOOK_SECRET`, database password, API keys
- During rotation grace period, accept signatures from both old and new secrets
- Log secret rotation events (without logging the actual secrets)

### Key Files

- `src/secrets.rs` — add refresh logic
- `src/config.rs` — make secrets mutable (Arc<RwLock>)
- `src/middleware/auth.rs` — accept multiple valid secrets

### Validation

- Test: rotate secret in Vault, verify app picks it up without restart
- Test: requests signed with old secret are accepted during grace period


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Input Sanitization for GraphQL Queries

**Complexity: High (200 points)**

### Description

The GraphQL resolvers may be vulnerable to query complexity attacks (deeply nested queries, alias bombs) that exhaust server resources.

### Requirements and Context

- Set query depth limit: max 10 levels
- Set query complexity limit: max 1000 points
- Limit number of aliases per query: max 20
- Use `async-graphql` built-in validation features
- Log rejected queries with complexity scores

### Key Files

- `src/graphql/schema.rs` — add limits to schema build
- `src/graphql/resolvers/` — review resolver complexity

### Validation

- Test: deeply nested query is rejected with 400
- Test: normal query passes


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: ERROR HANDLING & RESILIENCE
     ========================================================================== -->

## Implement Graceful Shutdown with In-Flight Request Draining

**Complexity: High (200 points)**

### Description

The `ReadinessState` (`src/readiness.rs`) supports connection draining conceptually, but the actual shutdown flow in `main.rs` does not use it. The server stops immediately on SIGTERM.

### Requirements and Context

- Register SIGTERM and SIGINT handlers
- On signal: set readiness to `not_ready`, wait for drain timeout (default 30s), then stop
- Use `axum::Server::with_graceful_shutdown` to stop accepting new connections
- Stop background tasks (processor, settlement, webhook) gracefully
- Log the number of in-flight requests at shutdown start

### Key Files

- `src/main.rs` — shutdown handling
- `src/readiness.rs` — drain state
- `src/services/scheduler.rs` — use `stop()` method

### Validation

- Test: SIGTERM triggers drain period
- Test: in-flight request completes before shutdown
- Test: new requests during drain get 503


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Retry Logic with Jitter to Transient Database Errors

**Complexity: High (200 points)**

### Description

Database operations fail immediately on transient errors (connection reset, deadlock). Adding automatic retry with jitter would improve reliability.

### Requirements and Context

- Create a `retry_with_backoff` utility function
- Retry on: connection reset, deadlock detected, serialization failure
- Max retries: 3, with exponential backoff + jitter
- Do not retry on: unique constraint violation, syntax error, not found
- Wrap `insert_transaction` and `update_transaction` with retry logic
- Track `db_retry_total` metric by error type

### Key Files

- New: `src/utils/retry.rs`
- `src/db/queries.rs` — wrap critical queries

### Validation

- Test: transient error triggers retry and succeeds
- Test: permanent error is not retried


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Structured Error Responses with Request Context

**Complexity: High (200 points)**

### Description

Error responses include `error`, `code`, and `status` but lack request context. Debugging requires correlating error responses with server logs.

### Requirements and Context

- Add `request_id` field to error responses
- Add `timestamp` field
- Add `detail` field for actionable information (e.g., "amount must be a positive number, received: -5.00")
- Add optional `docs_url` field pointing to error documentation
- Example: `{"error": "...", "code": "ERR_VALIDATION_001", "request_id": "abc-123", "docs_url": "/errors#ERR_VALIDATION_001"}`

### Key Files

- `src/error.rs` — `IntoResponse` implementation
- `src/middleware/request_logger.rs` — attach request ID to response

### Validation

- Test: error responses include request_id and timestamp
- Test: docs_url points to valid error catalog entry


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Panic Recovery Middleware

**Complexity: High (200 points)**

### Description

If a handler panics, the Tokio task crashes and the connection is dropped without a proper response. The client sees a connection reset.

### Requirements and Context

- Add `tower::catch_panic` or custom panic hook middleware
- On panic: return 500 with a generic error response
- Log the panic with full backtrace at `error` level
- Track `handler_panic_total` metric
- Include the panicking handler name if possible

### Key Files

- `src/main.rs` or `src/lib.rs` — add middleware layer

### Validation

- Test: handler panic returns 500 instead of connection reset
- Verify panic is logged with backtrace


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Transaction Status Transition Validation

**Complexity: High (200 points)**

### Description

There are no guards on status transitions. Code can set any status from any other status (e.g., `completed` → `pending`), creating invalid states.

### Requirements and Context

- Define a state machine: `pending` → `processing` → `completed`/`failed`; `failed` → `pending` (reprocess)
- Create `validate_status_transition(from, to) -> Result<(), AppError>` function
- Call this function in every status update query
- Return `AppError::InvalidStatusTransition` for invalid transitions
- Document the state machine in `docs/state-machine.md`

### Key Files

- `src/services/transaction_processor.rs` — status updates
- `src/services/processor.rs` — status updates
- `src/services/account_monitor.rs` — status updates
- `docs/state-machine.md` — existing documentation

### Validation

- Test: valid transitions succeed
- Test: invalid transitions (completed→pending) return error


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: TESTING INFRASTRUCTURE
     ========================================================================== -->

## Create Test Fixture Factory for Transaction Model

**Complexity: High (200 points)**

### Description

Test files repeat transaction creation boilerplate (see `src/db/models.rs` tests, `src/handlers/webhook.rs` tests). A shared fixture factory would reduce duplication and improve test readability.

### Requirements and Context

- Create `tests/fixtures.rs` with builder patterns:
  ```rust
  TransactionFixture::new()
      .with_status("completed")
      .with_amount("500.00")
      .build()
  ```
- Support all optional fields with sensible defaults
- Include common scenarios: `pending_deposit()`, `completed_withdrawal()`, `failed_transaction()`
- Usable from both unit tests and integration tests

### Key Files

- New: `tests/fixtures.rs`
- Update existing tests to use fixtures

### Validation

- All existing tests pass with the new fixtures
- At least 5 tests are refactored to use the factory


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Property-Based Testing for Validation Functions

**Complexity: High (200 points)**

### Description

Validation functions (`src/validation/`) use hand-written test cases that may miss edge cases. Property-based testing would systematically explore the input space.

### Requirements and Context

- Use `proptest` or `quickcheck` crate
- Property tests for: `validate_stellar_address`, `validate_asset_code`, `validate_positive_amount`, `sanitize_string`
- Properties: "valid inputs are accepted", "invalid inputs are rejected", "sanitization is idempotent"
- Generate adversarial inputs: unicode, control chars, very long strings, empty strings

### Key Files

- `src/validation/mod.rs` — validation functions
- `src/utils/sanitize.rs` — sanitization
- `Cargo.toml` — add dev dependency

### Validation

- Property tests discover at least one edge case not covered by existing tests
- All property tests pass


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Integration Test Harness with Automatic Database Setup

**Complexity: High (200 points)**

### Description

Integration tests require manual database setup (`setup_test_db` is duplicated across test files). Create a shared test harness that automatically provisions a test database.

### Requirements and Context

- Use `testcontainers` (already in dev-dependencies) to spin up Postgres and Redis
- Create a `TestApp` struct that holds pool, Redis client, and HTTP client
- Automatic migration and partition creation
- Automatic cleanup between tests (truncate tables)
- Support parallel test execution with separate databases

### Key Files

- New: `tests/common/mod.rs` — test harness
- Update: `tests/*.rs` — use `TestApp`

### Validation

- Integration tests run without manual database setup
- Tests are isolated and can run in parallel


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Load Testing Infrastructure with k6

**Complexity: High (200 points)**

### Description

The `docker-compose.load.yml` references k6 but there are no load test scripts defined.

### Requirements and Context

- Create k6 test scripts for:
  - `callback_load.js` — sustained callback ingestion (1000 req/s for 5 minutes)
  - `search_load.js` — concurrent search queries
  - `mixed_load.js` — realistic mix of callbacks, reads, and searches
- Define success criteria: p95 latency < 200ms, error rate < 0.1%
- Generate HTML report with results
- Document how to run load tests in `docs/load-test-results.md`

### Key Files

- New: `tests/load/callback_load.js`
- New: `tests/load/search_load.js`
- New: `tests/load/mixed_load.js`
- `docker-compose.load.yml` — k6 service

### Validation

- Load tests execute against docker-compose environment
- Results are generated and readable


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add End-to-End Test for Full Transaction Lifecycle

**Complexity: High (200 points)**

### Description

There is no single test that exercises the complete flow: callback received → transaction persisted → processor picks up → status updated → webhook delivered.

### Requirements and Context

- Use the `TestApp` harness to spin up all services
- POST a callback, verify 201
- Poll until processor updates status to `completed`
- Verify webhook delivery was attempted (use a mock HTTP server)
- Verify audit log entries exist for each status change
- Verify WebSocket notification was broadcast

### Key Files

- New: `tests/lifecycle_test.rs`

### Validation

- Test passes reliably (not flaky)
- Covers all state transitions


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Benchmark Tests for Critical Code Paths

**Complexity: High (200 points)**

### Description

There are no benchmarks to catch performance regressions. Critical paths (callback handling, transaction processing, query cache) should have established baselines.

### Requirements and Context

- Use `criterion` crate for statistically-rigorous benchmarks
- Benchmark: callback payload validation, transaction insertion, search query construction, HMAC signing
- Store baseline results in the repository
- CI can optionally run benchmarks and compare against baseline

### Key Files

- New: `benches/` directory with benchmark files
- `Cargo.toml` — add criterion

### Validation

- Benchmarks run and produce reports
- Results show consistent performance (low variance)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: MULTI-TENANT ISOLATION
     ========================================================================== -->

## Implement Row-Level Security for Multi-Tenant Data Isolation

**Complexity: High (200 points)**

### Description

The multi-tenant module (`src/Multi-Tenant Isolation Layer (Architecture)/`) defines a `TenantConfig` but there is no row-level data isolation. Tenants can query each other's transactions.

### Requirements and Context

- Add `tenant_id UUID` column to `transactions` table
- Create PostgreSQL Row-Level Security (RLS) policy
- Set `current_setting('app.tenant_id')` on each connection from the pool
- Enforce RLS in all queries without requiring code changes
- Admin users bypass RLS for cross-tenant operations

### Key Files

- `migrations/` — add column, create RLS policy
- `src/db/queries.rs` — set tenant context on connections
- `src/middleware/auth.rs` — extract tenant ID

### Validation

- Test: tenant A cannot see tenant B's transactions
- Test: admin can see all transactions
- Test: existing single-tenant queries work (tenant_id defaults to null)


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Tenant-Specific Rate Limiting and Quota Management

**Complexity: High (200 points)**

### Description

Rate limiting is global, not per-tenant. A noisy tenant can exhaust rate limits for everyone.

### Requirements and Context

- Wire tenant ID into `QuotaManager` key: `quota:usage:{tenant_id}`
- Load tenant-specific limits from `tenants.rate_limit_per_minute`
- Support per-tenant custom limits overriding the tier defaults
- Admin API to view/modify tenant quotas
- Dashboard showing quota usage per tenant

### Key Files

- `src/middleware/quota.rs` — tenant-scoped keys
- `src/db/queries.rs` — tenant config loading

### Validation

- Test: two tenants have independent quotas
- Test: exceeding one tenant's quota doesn't affect others


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Tenant Configuration Hot-Reload

**Complexity: High (200 points)**

### Description

Tenant configs are loaded into memory (`AppState::tenant_configs`) but there is no mechanism to refresh them without restarting the application.

### Requirements and Context

- Add a background task that reloads tenant configs every 60 seconds
- Add admin endpoint `POST /admin/tenants/reload` for immediate reload
- Log when configs change (new tenants added, limits changed)
- Use `tokio::sync::RwLock` (already in place) for concurrent read access

### Key Files

- `src/lib.rs` — `load_tenant_configs` method exists
- `src/main.rs` — spawn reload task

### Validation

- Test: adding a new tenant in DB is reflected within 60 seconds
- Test: admin reload endpoint triggers immediate refresh


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: API IMPROVEMENTS
     ========================================================================== -->

## Implement API Versioning with V1/V2 Route Groups

**Complexity: High (200 points)**

### Description

The handlers have `v1` and `v2` directories (`src/handlers/v1/`, `src/handlers/v2/`) but API versioning is not wired up. All requests go to unversioned routes.

### Requirements and Context

- Mount V1 routes at `/api/v1/*` and V2 routes at `/api/v2/*`
- Default (no version prefix) routes to latest version (V2)
- V1 routes should remain stable — never break backward compatibility
- Add `API-Version` response header indicating which version handled the request
- Version negotiation via `Accept-Version` header

### Key Files

- `src/handlers/v1/` — V1 handlers
- `src/handlers/v2/` — V2 handlers
- `src/lib.rs` — `create_app` function
- `src/middleware/versioning.rs` — existing versioning middleware

### Validation

- Test: `/api/v1/transactions` and `/api/v2/transactions` return correct versions
- Test: unversioned routes default to V2


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add OpenAPI Specification Auto-Generation from Code

**Complexity: High (200 points)**

### Description

The `ApiDoc` struct in `main.rs` manually lists all paths and schemas. When new endpoints are added, the OpenAPI spec falls out of sync.

### Requirements and Context

- Ensure all handlers have `#[utoipa::path]` annotations (some are missing)
- Auto-serve OpenAPI JSON at `/api/docs/openapi.json`
- Serve Swagger UI at `/api/docs` using `utoipa-swagger-ui`
- Include request/response examples in the spec
- Add the OpenAPI generation step to CI

### Key Files

- `src/main.rs` — `ApiDoc` struct
- `src/handlers/` — add missing annotations
- `Cargo.toml` — `utoipa-swagger-ui` already in dependencies

### Validation

- `/api/docs` loads Swagger UI in browser
- All endpoints appear in the OpenAPI spec
- Try-it-out works for health endpoint


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Cursor-Based Pagination for All List Endpoints

**Complexity: High (200 points)**

### Description

The settlements list endpoint (`src/db/queries.rs:list_settlements`) uses offset-based pagination (`LIMIT/OFFSET`), while transactions use cursor-based pagination. This inconsistency causes issues with large datasets.

### Requirements and Context

- Convert `list_settlements` to cursor-based pagination using `(created_at, id)` cursor
- Reuse the `cursor_util::encode/decode` functions from `src/utils/cursor.rs`
- Update the handler to return `next_cursor` and `has_more` metadata
- Apply the same pattern to audit logs listing

### Key Files

- `src/db/queries.rs` — `list_settlements`, `get_audit_logs`
- `src/handlers/settlements.rs` — update handler
- `src/utils/cursor.rs` — reuse encoding

### Validation

- Test: forward and backward pagination works for settlements
- Test: cursor stability across page loads


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Transaction Filtering by Date Range to List Endpoint

**Complexity: High (200 points)**

### Description

The transaction list endpoint only supports cursor-based pagination. Users cannot filter by date range without using the search endpoint.

### Requirements and Context

- Add optional `from_date` and `to_date` query parameters to `GET /transactions`
- Parse ISO 8601 date strings
- Apply as `WHERE created_at >= $from AND created_at < $to`
- Combine with existing cursor pagination
- Validate: `from_date` must be before `to_date`

### Key Files

- `src/handlers/webhook.rs` — `ListQuery` struct and `list_transactions`
- `src/db/queries.rs` — `list_transactions` query

### Validation

- Test: date-filtered list returns only transactions in range
- Test: invalid date format returns 400


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Bulk Transaction Status Update Endpoint

**Complexity: High (200 points)**

### Description

There is no way to update the status of multiple transactions in a single request. Admin operations require individual API calls.

### Requirements and Context

- New endpoint: `PATCH /admin/transactions/bulk-status`
- Request body: `{"transaction_ids": [...], "status": "failed", "reason": "manual override"}`
- Validate status transitions for each transaction
- Use a single SQL statement with `UPDATE ... WHERE id = ANY($1)`
- Audit log each update
- Return summary: `{"updated": 5, "failed": 2, "errors": [...]}`

### Key Files

- New handler in `src/handlers/admin/`
- `src/db/queries.rs` — bulk update query

### Validation

- Test: bulk update of 10 transactions succeeds
- Test: invalid status transition for one doesn't block others
- Test: audit logs created for each update


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add GraphQL Subscription for Real-Time Transaction Updates

**Complexity: High (200 points)**

### Description

The WebSocket handler (`src/handlers/ws.rs`) and broadcast channel exist but are not integrated with the GraphQL schema. No real-time subscription endpoint is available.

### Requirements and Context

- Implement `async-graphql` subscription for `transactionStatusChanged`
- Filter by tenant ID, asset code, or transaction ID
- Use the existing `tx_broadcast` channel as the event source
- Handle WebSocket disconnects gracefully
- Rate limit subscription creation per client

### Key Files

- `src/graphql/schema.rs` — add subscription root
- `src/graphql/resolvers/` — subscription resolver
- `src/handlers/ws.rs` — existing WebSocket setup

### Validation

- Test: subscribe, trigger status change, receive update via GraphQL
- Test: subscriber disconnect doesn't crash the server


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: SETTLEMENT & RECONCILIATION
     ========================================================================== -->

## Implement Settlement Dispute Resolution Workflow

**Complexity: High (200 points)**

### Description

Settlements can only be `completed`. There is no way to dispute, adjust, or void a settlement.

### Requirements and Context

- Add settlement statuses: `pending_review`, `disputed`, `adjusted`, `voided`
- Admin endpoint to change settlement status with a reason
- Voiding a settlement releases transactions back to `unsettled`
- Adjusting a settlement updates the total while preserving the original
- All status changes must be audit-logged

### Key Files

- `src/services/settlement.rs` — status management
- `src/db/models.rs` — Settlement model
- `src/handlers/admin/` — admin endpoints

### Validation

- Test: settlement dispute → review → resolution flow
- Test: voided settlement releases transactions


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Automated Reconciliation Scheduling

**Complexity: High (200 points)**

### Description

Reconciliation (`src/services/reconciliation.rs`) is only triggered manually via CLI. There should be automated daily reconciliation.

### Requirements and Context

- Register reconciliation as a scheduled job using `JobScheduler`
- Schedule: daily at 02:00 UTC
- Reconcile the previous 24-hour period
- Store reconciliation reports in the `reconciliation_reports` table (new migration)
- Alert (log + webhook) if discrepancies are found

### Key Files

- `src/services/reconciliation.rs` — reconciliation logic
- `src/services/scheduler.rs` — job registration
- `migrations/` — new table for reports

### Validation

- Test: scheduled job triggers reconciliation
- Test: report is stored correctly


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Reconciliation Report API Endpoint

**Complexity: High (200 points)**

### Description

Reconciliation reports are only available via CLI output. Add API endpoints for querying historical reports.

### Requirements and Context

- `GET /admin/reconciliation/reports` — list reports with pagination
- `GET /admin/reconciliation/reports/:id` — get a specific report
- `POST /admin/reconciliation/run` — trigger ad-hoc reconciliation
- Include summary stats: mismatch count, missing count, total amounts
- Admin auth required

### Key Files

- New: `src/handlers/admin/reconciliation.rs`
- `src/services/reconciliation.rs` — existing service

### Validation

- Test: list reports returns recent reconciliation results
- Test: triggering reconciliation via API works


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Settlement Batch Size Limits and Configuration

**Complexity: High (200 points)**

### Description

The settlement service processes all unsettled transactions in one batch. For assets with millions of transactions, this could create massive settlements and OOM errors.

### Requirements and Context

- Add `SETTLEMENT_MAX_BATCH_SIZE` config (default 10000)
- If more transactions are available, create multiple settlements for the same period
- Add `SETTLEMENT_MIN_TX_COUNT` (default 1) — don't create settlements for trivial amounts
- Log settlement batch size and total amount

### Key Files

- `src/services/settlement.rs` — batch limiting
- `src/config.rs` — new config

### Validation

- Test: large batch is split into multiple settlements
- Test: trivial batch below minimum is skipped


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: OPERATIONS & DEPLOYMENT
     ========================================================================== -->

## Implement Rolling Restart Support with Connection Draining

**Complexity: High (200 points)**

### Description

During deployments, instances are killed and restarted. Without proper draining, in-flight requests are dropped.

### Requirements and Context

- Implement Kubernetes preStop hook that calls `POST /admin/drain`
- The drain endpoint sets readiness to false and starts the drain timer
- The readiness probe (`/ready`) returns 503 during drain
- After drain timeout, the process exits cleanly
- Document the Kubernetes deployment spec in `docs/`

### Key Files

- `src/readiness.rs` — drain logic
- `src/main.rs` — graceful shutdown
- New: `docs/deployment.md`

### Validation

- Test: calling `/admin/drain` starts drain period
- Test: `/ready` returns 503 during drain


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Docker Health Check to Dockerfile

**Complexity: High (200 points)**

### Description

The `dockerfile` has no `HEALTHCHECK` instruction. Docker cannot determine if the application is healthy.

### Requirements and Context

- Add `HEALTHCHECK` instruction that curls the `/health` endpoint
- Interval: 30s, timeout: 5s, retries: 3
- Start period: 10s (allow startup time)
- Use `wget` instead of `curl` for smaller image size (Debian slim)

### Key Files

- `dockerfile` — add HEALTHCHECK

### Validation

- Docker reports container as healthy after startup
- Container marked unhealthy if app crashes


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Create Docker Compose Development Environment with Hot Reload

**Complexity: High (200 points)**

### Description

The current `docker-compose.yml` builds the full binary. For development, developers need hot-reload capability.

### Requirements and Context

- Add `docker-compose.dev.yml` with volume-mounted source code
- Use `cargo-watch` for hot-reload: rebuild on file changes
- Mount `src/` and `migrations/` as volumes
- Expose debug port for IDE debugger attachment
- Include pgAdmin or Adminer for database inspection

### Key Files

- New: `docker-compose.dev.yml`
- `docs/setup.md` — document dev workflow

### Validation

- Source code changes trigger automatic rebuild
- Developer can connect IDE debugger to running container


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Configuration Validation on Startup

**Complexity: High (200 points)**

### Description

The config module (`src/config.rs`) loads environment variables but has minimal validation. Invalid configurations (e.g., non-URL database string, negative rate limits) cause cryptic runtime errors.

### Requirements and Context

- Validate `DATABASE_URL` is a valid PostgreSQL connection string
- Validate `REDIS_URL` is a valid Redis connection string
- Validate `STELLAR_HORIZON_URL` is a valid HTTPS URL
- Validate rate limits are positive integers
- Validate port number is in valid range (1-65535)
- Print all validation errors at once, not one at a time

### Key Files

- `src/config.rs` — add validation
- `src/startup.rs` — existing startup validation

### Validation

- Test: invalid DATABASE_URL produces clear error
- Test: all valid configs pass validation


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Environment-Specific Configuration Profiles

**Complexity: High (200 points)**

### Description

There is no way to load different configurations for development, staging, and production beyond setting individual env vars.

### Requirements and Context

- Support `APP_ENV` variable: `development`, `staging`, `production`
- Load `.env.{APP_ENV}` file in addition to `.env`
- Development defaults: verbose logging, relaxed rate limits, longer timeouts
- Production defaults: JSON logging, strict rate limits, short timeouts
- Log which profile is active on startup

### Key Files

- `src/config.rs` — profile loading
- New: `.env.development`, `.env.production` examples

### Validation

- Test: setting `APP_ENV=production` loads production defaults
- Test: env vars override profile defaults


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Blue-Green Deployment Migration Safety Check

**Complexity: High (200 points)**

### Description

During blue-green deployments, both old and new versions may run simultaneously. Migrations that add required columns or change schemas break the old version.

### Requirements and Context

- Add a migration compatibility checker that verifies:
  - No `NOT NULL` columns added without defaults
  - No columns renamed (use add+migrate+drop pattern)
  - No table drops (add deprecation period)
- Run checker in CI before allowing migration merge
- Document safe migration patterns in `docs/`

### Key Files

- New: scripts to analyze migration SQL
- `.github/workflows/rust.yml` — add check step
- New: `docs/migration-safety.md`

### Validation

- CI blocks a PR that adds a NOT NULL column without a default
- Safe migrations pass the check


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: DOCUMENTATION
     ========================================================================== -->

## Create Comprehensive API Documentation with Examples

**Complexity: High (200 points)**

### Description

The existing docs cover architecture and individual features but lack a unified API reference with request/response examples.

### Requirements and Context

- Document every API endpoint with: URL, method, headers, request body, response body, error codes
- Include `curl` examples for each endpoint
- Organize by functional area: transactions, settlements, webhooks, admin
- Include authentication requirements for each endpoint
- Add rate limit information

### Key Files

- New: `docs/api-reference.md`

### Validation

- Every endpoint in `src/lib.rs:create_app` has documentation
- curl examples are copy-pasteable and work against local dev


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Contributing Guide with Architecture Decision Records

**Complexity: High (200 points)**

### Description

The README mentions contributing but there is no detailed guide. New contributors need to understand architectural decisions and coding conventions.

### Requirements and Context

- Create `CONTRIBUTING.md` with:
  - Development setup instructions
  - Code style guide (Rust conventions, error handling patterns)
  - PR process and review expectations
  - Testing requirements (unit, integration, benchmarks)
  - Architecture Decision Records (ADRs) for major decisions
- Add ADRs for: partitioning strategy, circuit breaker choice, multi-tenant approach

### Key Files

- New: `CONTRIBUTING.md`
- New: `docs/adr/` directory

### Validation

- A new contributor can set up the project using only the guide
- ADRs document the rationale behind key technical decisions


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Document Disaster Recovery Procedures

**Complexity: High (200 points)**

### Description

The backup service exists but there is no runbook for disaster recovery scenarios.

### Requirements and Context

- Document procedures for:
  - Complete database recovery from backup
  - Partial table recovery
  - Redis data loss recovery
  - Application crash and restart
  - Multi-region failover procedure
- Include estimated recovery times for each scenario
- List monitoring alerts and escalation procedures

### Key Files

- New: `docs/disaster-recovery.md`

### Validation

- Procedures are specific enough to follow step-by-step
- Recovery times are realistic estimates


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Create Runbook for Common Operational Tasks

**Complexity: High (200 points)**

### Description

Operational tasks (manual reconciliation, DLQ management, partition maintenance) require knowledge of internal systems. Create a runbook for operators.

### Requirements and Context

- Runbook entries for:
  - Manually triggering reconciliation
  - Inspecting and retrying DLQ entries
  - Creating/detaching partitions
  - Rotating secrets
  - Scaling the processor pool
  - Investigating slow queries
  - Resetting circuit breakers
- Each entry: problem description, diagnosis steps, resolution, rollback

### Key Files

- New: `docs/runbook.md`

### Validation

- Covers all CLI commands (`synapse-core tx`, `synapse-core backup`, etc.)
- Includes monitoring queries for common issues


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: STELLAR INTEGRATION
     ========================================================================== -->

## Implement Stellar Transaction Verification in Processor

**Complexity: High (200 points)**

### Description

The processor has a `TODO: Implement transaction processing logic` comment (`src/services/processor.rs:60`). The core verification step — confirming the on-chain deposit matches the callback data — is not implemented.

### Requirements and Context

- For each pending transaction, verify against Stellar Horizon:
  1. Fetch the account's recent payments
  2. Find a payment matching the memo, amount, and asset code
  3. If found, update status to `completed`
  4. If not found after a timeout, move to DLQ
- Handle Horizon pagination for accounts with many payments
- Respect the circuit breaker when querying Horizon
- Track verification latency and match rate

### Key Files

- `src/services/processor.rs` — implement the TODO
- `src/stellar/client.rs` — may need new methods
- `src/services/account_monitor.rs` — overlapping logic to refactor

### Validation

- Integration test with mocked Horizon: matching payment completes transaction
- Test: non-matching payment moves to DLQ after timeout


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Stellar Transaction Streaming via Server-Sent Events

**Complexity: High (200 points)**

### Description

The `AccountMonitor` uses polling (`fetch_payments` with cursor-based pagination). Stellar Horizon supports streaming via SSE for real-time payment detection with lower latency.

### Requirements and Context

- Implement SSE client for Horizon payments endpoint
- Use `reqwest` with streaming response or `eventsource-client` crate
- Maintain persistent HTTP connection with automatic reconnection
- Fall back to polling on SSE connection failure
- Track stream health metrics: reconnections, lag

### Key Files

- `src/services/account_monitor.rs` — add streaming mode
- `src/stellar/client.rs` — streaming method

### Validation

- Test: SSE stream receives and processes new payments
- Test: stream reconnects on dropout
- Test: fallback to polling works


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Multi-Asset Support for Stellar Operations

**Complexity: High (200 points)**

### Description

The config has a single `STELLAR_HORIZON_URL` and the system monitors a single set of accounts. Production requires monitoring multiple assets across different anchor accounts.

### Requirements and Context

- Support a list of monitored accounts via `STELLAR_MONITOR_ACCOUNTS` (comma-separated)
- Per-account asset filtering: only process payments for specified asset codes
- Store monitored accounts in the database for runtime management
- Admin API to add/remove monitored accounts

### Key Files

- `src/services/account_monitor.rs` — multiple accounts
- `src/config.rs` — multi-account config
- `migrations/` — account monitor config table

### Validation

- Test: two accounts with different asset filters process correctly
- Test: adding a new account via admin API starts monitoring


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Horizon API Response Caching

**Complexity: High (200 points)**

### Description

Every account lookup calls Horizon directly. Account details rarely change and can be cached to reduce API calls and improve latency.

### Requirements and Context

- Cache `get_account` responses in Redis with a 60-second TTL
- Cache key: `horizon:account:{address}`
- Skip cache for requests with `Cache-Control: no-cache` header
- Invalidate cache when a transaction for that account is completed
- Track cache hit rate

### Key Files

- `src/stellar/client.rs` — add caching layer
- `src/services/query_cache.rs` — reuse cache infrastructure

### Validation

- Test: second request for same account hits cache
- Test: cache invalidation works


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: BACKUP & DISASTER RECOVERY
     ========================================================================== -->

## Implement Point-in-Time Recovery Support

**Complexity: High (200 points)**

### Description

The backup service (`src/services/backup.rs`) creates full backups only. Point-in-time recovery requires WAL archiving or incremental backups.

### Requirements and Context

- Configure PostgreSQL WAL archiving in `docker-compose.yml` (for dev)
- Implement `pg_basebackup`-based full backup (instead of `pg_dump`)
- Support recovery to a specific timestamp
- Add CLI command: `synapse-core backup restore --timestamp "2026-01-15T10:30:00Z"`
- Document PITR setup for production PostgreSQL

### Key Files

- `src/services/backup.rs` — new recovery mode
- `src/cli.rs` — new CLI command
- `docker-compose.yml` — WAL archiving config

### Validation

- Test: restore to a timestamp between two backups
- Document recovery time for various data sizes


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Backup Verification via Automated Restore Test

**Complexity: High (200 points)**

### Description

Backups are created and checksummed but never verified by actually restoring them. A corrupt backup is only discovered during a disaster.

### Requirements and Context

- Weekly scheduled job that:
  1. Creates a temporary test database
  2. Restores the latest backup to it
  3. Runs verification queries (row counts, recent timestamp check)
  4. Reports success/failure
  5. Drops the test database
- Store verification results in a log table
- Alert on verification failure

### Key Files

- `src/services/backup.rs` — add verification method
- `src/services/scheduler.rs` — schedule weekly verification

### Validation

- Test: verification detects a corrupted backup (tampered checksum)
- Test: successful verification is logged


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Backup Progress Reporting

**Complexity: High (200 points)**

### Description

The backup process runs synchronously with no progress reporting. Large databases take minutes to backup with no visibility.

### Requirements and Context

- Estimate backup size from `pg_database_size()`
- Report progress at 25%, 50%, 75%, 100%
- Expose backup status via admin endpoint: `GET /admin/backup/status`
- Return: current phase, progress percentage, elapsed time, estimated remaining

### Key Files

- `src/services/backup.rs` — progress tracking
- `src/handlers/admin/` — status endpoint

### Validation

- Test: progress updates appear in logs during backup
- Test: status endpoint returns correct phase


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: FEATURE FLAGS & CONFIGURATION
     ========================================================================== -->

## Implement Feature Flag Percentage Rollout

**Complexity: High (200 points)**

### Description

The `FeatureFlagService` (`src/services/feature_flags.rs`) supports boolean flags only. Gradual rollouts require percentage-based activation.

### Requirements and Context

- Add `rollout_percentage INT` column to `feature_flags` table (0-100)
- For percentage flags, hash `tenant_id + flag_name` and check if hash % 100 < rollout_percentage
- This ensures consistent activation per tenant (same tenant always gets the same result)
- Admin API to adjust rollout percentage
- Track activation rate per flag

### Key Files

- `src/services/feature_flags.rs` — percentage logic
- `migrations/` — add column
- `src/handlers/admin/` — management endpoint

### Validation

- Test: 50% rollout activates for approximately half of test tenants
- Test: same tenant always gets the same result


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Feature Flag Audit Trail

**Complexity: High (200 points)**

### Description

Feature flag changes are not tracked. There is no way to know who changed a flag or when.

### Requirements and Context

- Log all flag changes to the `audit_logs` table
- Include: old value, new value, actor (admin user), timestamp
- Add `GET /admin/feature-flags/history` endpoint
- Support filtering by flag name and date range

### Key Files

- `src/services/feature_flags.rs` — audit logging on change
- `src/db/audit.rs` — audit log utilities
- New handler in `src/handlers/admin/`

### Validation

- Test: changing a flag creates an audit log entry
- Test: history endpoint returns changes


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Feature Flag Dependencies

**Complexity: High (200 points)**

### Description

Some features depend on others (e.g., "advanced settlement" requires "settlement"). There is no way to express or enforce these dependencies.

### Requirements and Context

- Add `depends_on TEXT[]` column to `feature_flags` table
- When checking a flag, recursively check dependencies
- Prevent enabling a flag if dependencies are disabled
- Prevent disabling a flag that is a dependency of an enabled flag
- Visualize dependency graph in admin API response

### Key Files

- `src/services/feature_flags.rs` — dependency resolution
- `migrations/` — add column

### Validation

- Test: enabling a flag with disabled dependency fails
- Test: disabling a flag that is depended on fails


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: PERFORMANCE OPTIMIZATION
     ========================================================================== -->

## Implement Query Result Streaming for Large Exports

**Complexity: High (200 points)**

### Description

The export handler (`src/handlers/export.rs`) loads all matching transactions into memory before writing to CSV. For millions of rows, this causes OOM.

### Requirements and Context

- Use `sqlx::query_as(...).fetch(pool)` stream instead of `fetch_all`
- Write CSV rows incrementally using `tokio::io::AsyncWrite`
- Set `Transfer-Encoding: chunked` response header
- Add `Content-Disposition: attachment` header
- Backpressure: pause reading from DB if the client is slow

### Key Files

- `src/handlers/export.rs` — streaming refactor

### Validation

- Test: export 1M rows without OOM (memory stays under 100MB)
- Test: interrupted download doesn't leak connections


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add In-Memory LRU Cache Layer Before Redis

**Complexity: High (200 points)**

### Description

Every cache lookup hits Redis over the network. For frequently-accessed data (status counts, asset stats), an in-memory LRU cache would reduce latency.

### Requirements and Context

- Implement two-tier cache: in-memory LRU → Redis → database
- In-memory cache with configurable size (default 1000 entries) and TTL (default 30s)
- When Redis is unavailable, serve from in-memory cache
- Cache invalidation must clear both tiers
- Track hit rates for each tier separately

### Key Files

- `src/services/query_cache.rs` — add LRU layer
- `Cargo.toml` — add `lru` crate

### Validation

- Test: hot data served from memory without Redis call
- Test: invalidation clears both tiers
- Benchmark: >5x latency improvement for cached reads


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Optimize Webhook Dispatcher Database Queries

**Complexity: High (200 points)**

### Description

The webhook dispatcher queries `SELECT * FROM webhook_endpoints WHERE id = $1` for every delivery attempt. This is N+1 query pattern.

### Requirements and Context

- Batch-load all relevant endpoints in a single query at the start of `process_pending`
- Use a HashMap for lookups during delivery processing
- Pre-filter disabled endpoints in the batch query
- Log query reduction metrics

### Key Files

- `src/services/webhook_dispatcher.rs` — `attempt_delivery` and `process_pending`

### Validation

- Test: N deliveries result in 2 queries (deliveries + endpoints) instead of N+1
- Monitor query count reduction


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Connection Pool Metrics Emission

**Complexity: High (200 points)**

### Description

The `pool_monitor_task` in `main.rs` logs pool stats but doesn't emit structured metrics that can be graphed.

### Requirements and Context

- Emit pool metrics every 30 seconds:
  - `db_pool_active_connections` (gauge)
  - `db_pool_idle_connections` (gauge)
  - `db_pool_max_connections` (gauge)
  - `db_pool_utilization_percent` (gauge)
  - `db_pool_wait_count` (counter) — connections waited for
- Use OpenTelemetry gauges if available, otherwise log structured JSON

### Key Files

- `src/main.rs` — `pool_monitor_task`
- `src/metrics.rs` — metric recording

### Validation

- Metrics appear in monitoring system
- Pool utilization tracks with actual load


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: CODE QUALITY & REFACTORING
     ========================================================================== -->

## Refactor Transaction Status as a Rust Enum Instead of String

**Complexity: High (200 points)**

### Description

Transaction status is a `String` field throughout the codebase. This allows typos, invalid states, and makes the state machine implicit. Refactor to a proper Rust enum.

### Requirements and Context

- Create `TransactionStatus` enum: `Pending`, `Processing`, `Completed`, `Failed`
- Implement `sqlx::Type`, `Serialize`, `Deserialize` for the enum
- Replace all string comparisons (`status == "pending"`) with enum matching
- Implement `Display` and `FromStr` for backward-compatible serialization
- Update GraphQL resolvers to use the enum

### Key Files

- `src/db/models.rs` — enum definition
- `src/services/processor.rs` — status comparisons
- `src/services/transaction_processor.rs` — status updates
- `src/handlers/webhook.rs` — default status
- `src/db/queries.rs` — query parameters

### Validation

- All existing tests pass without changes to test assertions
- Invalid status strings cause compile-time errors


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Extract Common SQL Queries into Typed Query Builder

**Complexity: High (200 points)**

### Description

The `search_transactions` function (`src/db/queries.rs:280-422`) constructs SQL strings dynamically with manual parameter binding. This is error-prone and hard to maintain.

### Requirements and Context

- Create a `TransactionQueryBuilder` struct with typed filter methods
- Methods: `.status()`, `.asset_code()`, `.amount_range()`, `.date_range()`, `.account()`, `.cursor()`
- Generate SQL and bind parameters safely
- Support `COUNT(*)` and `SELECT *` modes from the same builder
- Eliminate the duplicate parameter binding in search_transactions

### Key Files

- `src/db/queries.rs` — refactor `search_transactions`
- New: `src/db/query_builder.rs`

### Validation

- All search tests pass with the new builder
- Code is shorter and more readable


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Eliminate Dead Code and Unused Imports

**Complexity: High (200 points)**

### Description

The README notes "some warnings about unused imports or dead code are expected." These should be cleaned up as the codebase matures.

### Requirements and Context

- Run `cargo clippy -- -W dead_code -W unused_imports`
- Remove or `#[allow()]` annotate genuinely planned-but-unimplemented code
- Remove `.bak` files (e.g., `src/handlers/graphql.rs.bak`)
- Remove commented-out code blocks in `src/main.rs` (lines 206-212, 296-300)
- Document any code intentionally kept for future use

### Key Files

- Throughout `src/` — dead code cleanup
- `src/handlers/graphql.rs.bak` — delete
- `src/main.rs` — remove commented code

### Validation

- `cargo clippy -- -D warnings` passes with zero warnings
- No `.bak` files remain


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Unify Error Handling Pattern Across All Handlers

**Complexity: High (200 points)**

### Description

Error handling is inconsistent: some handlers return `AppError`, some return raw `impl IntoResponse`, and some use `.map_err(|e| AppError::DatabaseError(e.to_string()))` while others use the `From` impl.

### Requirements and Context

- All handlers should return `Result<impl IntoResponse, AppError>`
- Remove redundant `.map_err()` calls where `From` impls exist
- Add `From<anyhow::Error>` and `From<redis::RedisError>` impls to `AppError`
- Ensure all error paths include the stable error code

### Key Files

- `src/error.rs` — add From impls
- `src/handlers/*.rs` — normalize error returns

### Validation

- All handlers use consistent error pattern
- Error response format is identical across all endpoints


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Clippy Configuration with Project-Specific Lints

**Complexity: High (200 points)**

### Description

There is no `clippy.toml` or workspace-level clippy configuration. Custom lint rules should be enforced project-wide.

### Requirements and Context

- Create `clippy.toml` with project-specific settings
- Enable lints: `clippy::unwrap_used` (warn), `clippy::expect_used` (warn in production code), `clippy::todo` (deny)
- Allow: necessary exceptions (e.g., `unwrap` in test code)
- Add lint configuration to `Cargo.toml` under `[lints.clippy]`
- Document why specific lints are configured as they are

### Key Files

- New: `clippy.toml`
- `Cargo.toml` — lint configuration

### Validation

- `cargo clippy` enforces the configured lints
- CI runs clippy with the project configuration


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Normalize Module Path for Multi-Tenant Directory

**Complexity: High (200 points)**

### Description

The multi-tenant module uses a non-standard path with spaces and parentheses: `src/Multi-Tenant Isolation Layer (Architecture)/`. This causes issues with tools, scripts, and cross-platform compatibility.

### Requirements and Context

- Rename to `src/tenant/` (conventional Rust module naming)
- Update the `#[path = ...]` attribute in `src/lib.rs`
- Update all import paths throughout the codebase
- Ensure no file references break

### Key Files

- `src/lib.rs` — path attribute (line 16-17)
- `src/Multi-Tenant Isolation Layer (Architecture)/` — rename to `src/tenant/`

### Validation

- `cargo build` succeeds after rename
- No path references to old directory name remain


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: DEVELOPER EXPERIENCE
     ========================================================================== -->

## Add CLI Command for Local Development Seed Data

**Complexity: High (200 points)**

### Description

New developers must manually create test data. A seed command would populate the database with realistic sample data for development.

### Requirements and Context

- New CLI command: `synapse-core seed --count 1000`
- Generate realistic transactions with:
  - Random Stellar addresses
  - Various asset codes (USD, EUR, GBP)
  - Amount distribution: 80% small ($10-$500), 15% medium ($500-$5000), 5% large ($5000+)
  - Mix of statuses: 60% completed, 20% pending, 15% processing, 5% failed
  - Dates spread over the last 6 months
- Create corresponding settlements and audit logs
- Idempotent: running twice doesn't duplicate data

### Key Files

- `src/cli.rs` — add `Seed` command
- New: `src/services/seeder.rs`

### Validation

- `synapse-core seed --count 100` creates 100 transactions with realistic data
- Stats endpoints show meaningful data after seeding


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement `cargo xtask` for Common Development Commands

**Complexity: High (200 points)**

### Description

Developers need to run multiple commands for common tasks (setup, test, release). A `cargo xtask` pattern simplifies this.

### Requirements and Context

- Create workspace xtask crate with commands:
  - `cargo xtask setup` — start docker-compose, run migrations, seed data
  - `cargo xtask test` — run unit + integration tests
  - `cargo xtask lint` — fmt + clippy + audit
  - `cargo xtask release` — build, test, tag, push
- Each command with `--help` documentation

### Key Files

- New: `xtask/` crate
- `Cargo.toml` — workspace configuration

### Validation

- `cargo xtask setup` gets a new developer ready to run in < 2 minutes
- All xtask commands have help text


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Changelog Generation from Git History

**Complexity: High (200 points)**

### Description

There is no changelog. Contributors and users cannot see what changed between releases.

### Requirements and Context

- Use `git-cliff` or `conventional-changelog` for automated generation
- Configure conventional commit parsing
- Categories: Features, Bug Fixes, Performance, Breaking Changes
- Generate `CHANGELOG.md` on each release
- CI step to validate commit message format

### Key Files

- New: `cliff.toml` — git-cliff configuration
- New: `CHANGELOG.md`
- `.github/workflows/rust.yml` — add commit lint step

### Validation

- `git-cliff` generates a readable changelog from existing commits
- CI validates commit message format on PRs


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Create Developer Setup Script

**Complexity: High (200 points)**

### Description

Setting up the development environment requires multiple manual steps. Automate the setup process.

### Requirements and Context

- Create `scripts/setup.sh` that:
  1. Checks prerequisites (Rust, Docker, psql)
  2. Copies `.env.example` to `.env` if not exists
  3. Starts Docker services
  4. Waits for services to be healthy
  5. Runs migrations
  6. Verifies setup with a health check
- Support `--reset` flag to wipe and recreate everything
- Colorized output with clear status messages

### Key Files

- New: `scripts/setup.sh`
- Update: `docs/setup.md` — reference the script

### Validation

- Running `./scripts/setup.sh` on a clean machine gets everything working
- `--reset` flag cleans up existing data


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: WEBSOCKET & REAL-TIME
     ========================================================================== -->

## Implement WebSocket Authentication and Authorization

**Complexity: High (200 points)**

### Description

The WebSocket handler (`src/handlers/ws.rs`) accepts all connections without authentication. Any client can subscribe to all transaction updates.

### Requirements and Context

- Require authentication token in WebSocket upgrade request (query parameter or header)
- Validate token against tenant API keys
- Only broadcast events for the authenticated tenant's transactions
- Send an error frame and close on invalid auth
- Rate limit WebSocket connections per client IP

### Key Files

- `src/handlers/ws.rs` — authentication and filtering
- `src/middleware/auth.rs` — reuse auth logic

### Validation

- Test: unauthenticated WebSocket connection is rejected
- Test: authenticated client only receives their tenant's events


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add WebSocket Heartbeat and Connection Health Monitoring

**Complexity: High (200 points)**

### Description

WebSocket connections can silently drop. Without heartbeats, the server accumulates dead connections.

### Requirements and Context

- Send ping frames every 30 seconds
- Close connections that don't respond with pong within 10 seconds
- Track active WebSocket connection count metric
- Log connection open/close events with client info
- Expose connection count in `/health` response

### Key Files

- `src/handlers/ws.rs` — heartbeat logic

### Validation

- Test: dead connection is detected and closed within 40 seconds
- Test: active connection survives heartbeat


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement WebSocket Message Backpressure

**Complexity: High (200 points)**

### Description

The broadcast channel has a capacity of 100 (set in `main.rs:236`). Slow subscribers miss messages silently. There is no mechanism to detect or handle this.

### Requirements and Context

- Detect `RecvError::Lagged` on the broadcast receiver
- Send a "messages_dropped" notification to the client with the count of missed messages
- Offer a "resync" mechanism: client can request the latest N events from the database
- Track `ws_messages_dropped_total` metric per client

### Key Files

- `src/handlers/ws.rs` — lag detection
- `src/main.rs` — broadcast channel configuration

### Validation

- Test: slow client receives dropped message notification
- Test: resync request returns latest events


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: LOCK MANAGEMENT & CONCURRENCY
     ========================================================================== -->

## Add Distributed Lock Timeout Monitoring

**Complexity: High (200 points)**

### Description

The lock manager (`src/services/lock_manager.rs`) acquires Redis-based distributed locks but has no monitoring for lock contention or stale locks.

### Requirements and Context

- Track metrics: `lock_acquired_total`, `lock_contention_total`, `lock_hold_duration_ms`
- Alert on locks held longer than 2x their expected duration
- Log lock acquisition and release at `debug` level
- Add `GET /admin/locks` endpoint showing active locks

### Key Files

- `src/services/lock_manager.rs` — metrics and logging

### Validation

- Test: lock metrics are emitted
- Test: admin endpoint shows current locks


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Fair Lock Queuing for Worker Coordination

**Complexity: High (200 points)**

### Description

The current lock implementation uses a simple try-acquire model. If multiple workers contend for a lock, there is no fairness guarantee — a single worker could repeatedly win.

### Requirements and Context

- Implement Redis-based fair queue using sorted sets
- Workers enqueue themselves with a timestamp score
- Lock is granted to the oldest waiter
- Maximum wait time before giving up: configurable
- Prevent queue becoming stale if a waiter crashes

### Key Files

- `src/services/lock_manager.rs` — fair queue implementation

### Validation

- Test: with N workers, each gets approximately equal lock time
- Test: crashed waiter is cleaned up from queue


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: ASSET MANAGEMENT
     ========================================================================== -->

## Implement Asset Registry with Issuer Validation

**Complexity: High (200 points)**

### Description

The `Asset` model (`src/db/models.rs:349-360`) is a placeholder with `fetch_all` returning an empty vec. There is no asset registry to validate incoming transactions against.

### Requirements and Context

- Populate `assets` table from the existing migration (`20260219000000_add_assets_table.sql`)
- Implement `fetch_all` to actually query the database
- Validate incoming callbacks: only accept transactions for registered assets
- Add admin API for asset management (add, remove, enable/disable)
- Seed with default assets: USD, EUR, GBP and their Stellar issuers

### Key Files

- `src/db/models.rs` — implement `Asset::fetch_all`
- `src/config/assets.rs` — asset configuration
- `src/handlers/webhook.rs` — validate asset on callback
- `migrations/20260219000000_add_assets_table.sql` — existing migration

### Validation

- Test: callback with unregistered asset returns 400
- Test: callback with registered asset succeeds


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Asset-Level Configuration for Processing Rules

**Complexity: High (200 points)**

### Description

All assets are processed with the same rules. Different assets may need: different minimum amounts, different settlement schedules, different validation rules.

### Requirements and Context

- Add columns to `assets` table: `min_amount NUMERIC`, `max_amount NUMERIC`, `settlement_schedule VARCHAR`, `enabled BOOLEAN`
- Validate transaction amount against asset min/max
- Settlement service uses asset-specific schedule
- Disabled assets reject new callbacks
- Admin API to manage asset configs

### Key Files

- `migrations/` — alter assets table
- `src/handlers/webhook.rs` — amount validation
- `src/services/settlement.rs` — asset-specific scheduling

### Validation

- Test: transaction below asset minimum is rejected
- Test: disabled asset rejects callbacks


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: AUDIT & COMPLIANCE
     ========================================================================== -->

## Implement Audit Log Search and Export

**Complexity: High (200 points)**

### Description

Audit logs (`src/db/audit.rs`) are stored but only queryable by entity ID. Compliance requires searching across all entities and exporting logs.

### Requirements and Context

- Add search endpoint: `GET /admin/audit/search?actor=&action=&from_date=&to_date=&entity_type=`
- Support CSV export of search results
- Add pagination with cursor
- Response includes total count for the search criteria
- Admin auth required

### Key Files

- `src/db/audit.rs` — search query
- New: `src/handlers/admin/audit.rs`
- `src/db/queries.rs` — audit search query

### Validation

- Test: search by actor returns correct logs
- Test: CSV export is well-formatted


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Audit Log Retention Policy

**Complexity: High (200 points)**

### Description

Audit logs grow indefinitely. A retention policy should archive or delete old logs.

### Requirements and Context

- Default retention: 365 days
- Config: `AUDIT_LOG_RETENTION_DAYS`
- Background job (monthly) to delete logs older than retention period
- Before deletion, export to compressed archive file
- Never delete audit logs for disputed or flagged transactions

### Key Files

- `src/db/audit.rs` — retention logic
- `src/services/scheduler.rs` — schedule cleanup job

### Validation

- Test: logs older than retention are deleted
- Test: flagged transaction logs are preserved


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Compliance Report Generation

**Complexity: High (200 points)**

### Description

Compliance teams need periodic reports summarizing transaction volumes, settlement totals, and anomalies.

### Requirements and Context

- Generate reports for: daily, weekly, monthly periods
- Include: transaction volumes by asset, settlement totals, anomaly counts, top accounts by volume
- Store reports in database with a `compliance_reports` table
- API endpoint: `GET /admin/compliance/reports`
- Support PDF or JSON output format

### Key Files

- New: `src/services/compliance.rs`
- New: `src/handlers/admin/compliance.rs`
- `migrations/` — reports table

### Validation

- Test: daily report generation produces correct summaries
- Test: API endpoint returns historical reports


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     CATEGORY: CACHING IMPROVEMENTS
     ========================================================================== -->

## Implement Cache Stampede Prevention

**Complexity: High (200 points)**

### Description

When a popular cache key expires, all concurrent requests hit the database simultaneously (cache stampede). The `QueryCache` has no protection against this.

### Requirements and Context

- Implement "early expiry" pattern: refresh cache 10% before TTL expires
- Use Redis lock to ensure only one request refreshes the cache
- Other requests serve the stale value during refresh
- Track `cache_stampede_prevented` metric
- Configurable per cache key

### Key Files

- `src/services/query_cache.rs` — stampede prevention logic

### Validation

- Test: 100 concurrent requests for an expired key result in only 1 DB query
- Benchmark showing no latency spike on cache expiry


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Cache Key Namespacing for Multi-Tenant

**Complexity: High (200 points)**

### Description

Cache keys are global (e.g., `query:status_counts`). Multi-tenant mode requires tenant-scoped cache keys.

### Requirements and Context

- Prefix cache keys with tenant ID when in multi-tenant mode: `tenant:{id}:query:status_counts`
- Invalidation should clear both tenant-specific and global keys
- Backward compatible: no prefix when no tenant context
- Update all cache key generation functions

### Key Files

- `src/services/query_cache.rs` — key generation functions

### Validation

- Test: two tenants get independent cached results
- Test: single-tenant mode works without change


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Cache Warming on Partition Rotation

**Complexity: High (200 points)**

### Description

When a new month's partition is created, queries that span the partition boundary may be slow on first execution (cold cache). Pre-warm the cache after partition creation.

### Requirements and Context

- After `PartitionManager` creates a new partition, trigger cache warming
- Warm: status counts, daily totals (last 7 days), asset stats
- Log cache warming after partition rotation
- Reuse existing `warm_cache` method

### Key Files

- `src/db/partition.rs` — trigger warming after partition creation
- `src/services/query_cache.rs` — `warm_cache` method

### Validation

- Test: cache is warm after partition creation
- No extra warming if partition already exists


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

<!-- ==========================================================================
     LAST ISSUES
     ========================================================================== -->

## Add Request Tracing Through the Full Processing Pipeline

**Complexity: High (200 points)**

### Description

OpenTelemetry traces exist for individual handlers but don't follow a transaction through the asynchronous processing pipeline (callback → queue → processor → webhook delivery).

### Requirements and Context

- Store the trace context (trace_id, span_id) alongside the transaction in the database
- When the processor picks up a transaction, create a child span linked to the original
- Webhook delivery spans are linked to the processing span
- End-to-end trace from callback receipt to webhook delivery
- Include trace_id in webhook delivery headers (`X-Trace-Id`)

### Key Files

- `src/handlers/webhook.rs` — capture trace context on callback
- `src/services/processor.rs` — create linked spans
- `src/services/webhook_dispatcher.rs` — create linked spans
- `src/db/models.rs` — add `trace_id` column

### Validation

- Test: full trace appears in OTLP collector from callback to webhook
- Verify trace linking works across async boundaries


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Implement Automated Dependency Update Workflow

**Complexity: High (200 points)**

### Description

Dependencies are manually updated. There is no automated process to detect and propose updates.

### Requirements and Context

- Configure Dependabot or Renovate for `Cargo.toml` dependencies
- Group minor/patch updates into weekly PRs
- Major updates as individual PRs
- Auto-merge patch updates that pass CI
- Ignore: pinned dependencies with documented reasons

### Key Files

- New: `.github/dependabot.yml` or `renovate.json`

### Validation

- Bot creates PRs for outdated dependencies
- Auto-merge works for patch updates


### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`

---

## Add Resource Limits to Background Tasks

**Complexity: High (200 points)**

### Description

Background tasks (processor, settlement, webhook, partition manager) run unbounded. A runaway task could consume all CPU or memory.

### Requirements and Context

- Set maximum concurrent background tasks via Tokio semaphore
- Each background task should have a maximum execution time
- If a task exceeds its time limit, cancel and log an error
- Track `background_task_timeout_total` metric
- Defaults: processor=30s, settlement=120s, webhook=60s, partition=300s

### Key Files

- `src/main.rs` — background task spawning
- `src/services/processor.rs`, `settlement.rs`, `webhook_dispatcher.rs`

### Validation

- Test: task exceeding timeout is cancelled
- Test: semaphore prevents unbounded parallelism

### Guidelines

- **Target branch:** `develop` — all PRs must be opened against `develop`, not `main`
- **Branch from latest:** `git checkout develop && git pull && git checkout -b feat/your-feature`
- **Before pushing, you must run and pass all four checks:**
  ```bash
  cargo fmt --all -- --check
  cargo clippy -- -D warnings
  cargo build
  cargo test
  ```
- **Only modify files listed in Key Files** — out-of-scope changes will be rejected to prevent merge conflicts
- **Keep your branch rebased:** `git fetch origin && git rebase origin/develop`
- PR title: `feat: <issue title>` | PR body must include: `Closes #<issue_number>`
