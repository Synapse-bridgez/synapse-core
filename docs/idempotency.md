# Idempotency Implementation

## Overview

This implementation provides webhook idempotency using Redis to prevent duplicate transaction processing when webhooks are delivered multiple times due to network retries.

## How It Works

### 1. Idempotency Key
- Webhooks must include an `X-Idempotency-Key` header (typically the `anchor_transaction_id`)
- This key uniquely identifies each webhook request

### 2. Request Flow

#### First Request (New)
1. Client sends webhook with `X-Idempotency-Key: transaction-123`
2. Middleware checks Redis for key `idempotency:transaction-123`
3. Key doesn't exist → Set key to "PROCESSING" with 5-minute TTL
4. Process the webhook normally
5. On success (2xx response) → Store response in Redis with 24-hour TTL
6. On failure → Delete the key to allow retry

#### Duplicate Request (Processing)
1. Client sends same webhook while first is still processing
2. Middleware finds key with value "PROCESSING"
3. Return `429 Too Many Requests` with retry-after header
4. Client should wait and retry

#### Duplicate Request (Completed)
1. Client sends same webhook after successful processing
2. Middleware finds key with cached response
3. Return cached response (200 OK) with `cached: true` flag
4. No duplicate processing occurs

### 3. TTL Strategy
- **Processing Lock**: 5 minutes (prevents stuck locks from failed requests)
- **Completed Response**: 24 hours (prevents duplicate processing within reasonable window)

### 4. Database Fallback (Redis-primary, DB-fallback-during-outage, DB-consulted-on-recovery)

Redis is the primary store, but it isn't the only one — `check_idempotency` (`src/middleware/idempotency.rs`) has three paths, not one:

1. **Healthy Redis, cache hit** → return the cached response from Redis.
2. **Healthy Redis, cache miss** → before issuing a fresh Redis lock, consult the `idempotency_keys` Postgres table. If a row exists there (written during a prior Redis outage — see path 3), recognize it as `Completed`/`Processing` instead of treating the key as brand new. This is what makes a retry *after* Redis recovers, for a request that was originally recorded *during* an outage, come back as a duplicate instead of executing twice. Emits `idempotency_db_fallback_recovered_total` when this path finds a row.
3. **Redis unreachable** → fall back entirely to the `idempotency_keys` table: check for an existing row, or insert one with `status = 'processing'` (`lock_token: None`). `store_response` correspondingly writes to Postgres whenever `lock_token` is `None`.

Path 2 is the fix for the gap that used to exist here: before it was added, the healthy-path lookup only ever checked Redis, so a request recorded via path 3 during an outage was invisible once Redis recovered, and a caller's well-intentioned retry (the entire point of an idempotency key) would double-execute. See `tests/idempotency_recovery_test.rs` for the regression test driving this exact degraded→healthy→retry sequence.

Note the DB fallback path (`check_idempotency_key`/`insert_idempotency_key`) is keyed only by `key`, not `tenant_id` — the Redis path scopes by tenant (`idempotency:<tenant_id>:<key>`) but the `idempotency_keys` table has no tenant column. Two different tenants using the same literal key string during an outage would collide in the DB fallback where they wouldn't in Redis. This is a pre-existing narrower gap, not introduced or fixed by the DB-fallback-recovery change — worth knowing about, not addressed here.

### 5. Manually checking a key's DB-fallback record

For on-call triage of a reported duplicate action from a past Redis outage window, see the runbook's "Triaging a Reported Duplicate Action from a Past Redis Outage" section — short version: `SELECT * FROM idempotency_keys WHERE key = '<key>'`.

### 6. Batch Requests

Batch endpoints (batch quote, batch order submission, bulk cancel, bulk status) use the same idempotency mechanism, but at two levels:

- **Batch-level key**: the caller supplies one `X-Idempotency-Key` for the whole batch, exactly as for a single request. A retried batch with the same key is recognized as a duplicate and does not re-execute any item.
- **Per-item keys**: each item in the batch gets a deterministic idempotency key derived from the batch key plus the item's index, e.g. `batch:<batch_key>:<index>`. Because the derivation is deterministic, a retried batch (or a retry of a single item within a re-submitted batch) maps to the same per-item key and is deduplicated by the same Redis/DB machinery described above.

Consequences worth knowing:

- Retrying a batch after a partial failure is safe: items that already succeeded are recognized as duplicates and are not submitted twice; only the items that failed are re-executed.
- Reordering items within a batch changes their indices, and therefore their derived per-item keys. Callers that want retry deduplication to hold must keep item order stable across retries (or retry individual items using the single-item endpoints).
- The per-item keys are scoped by tenant in Redis (`idempotency:<tenant_id>:batch:<batch_key>:<index>`), so two tenants using the same batch key do not collide on the healthy path. The DB fallback path retains the pre-existing tenant-scoping gap noted in section 4.

## Configuration

### Environment Variables
```bash
REDIS_URL=redis://localhost:6379
```

### Docker Compose
Redis is automatically configured in `docker-compose.yml`:
```yaml
redis:
  image: redis:7-alpine
  ports:
    - "6379:6379"
```

## Usage

### Making Idempotent Webhook Requests

```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: anchor-tx-12345" \
  -d '{
    "id": "webhook-001",
    "anchor_transaction_id": "anchor-tx-12345"
  }'
```

### Response Scenarios

#### Success (First Request)
```json
{
  "success": true,
  "message": "Webhook webhook-001 processed successfully"
}
```
Status: `200 OK`

#### Processing (Duplicate During Processing)
```json
{
  "error": "Request is currently being processed",
  "retry_after": 5
}
```
Status: `429 Too Many Requests`

#### Cached (Duplicate After Completion)
```json
{
  "cached": true,
  "message": "Request already processed"
}
```
Status: `200 OK`

## Architecture

### Components

1. **IdempotencyService** (`src/middleware/idempotency.rs`)
   - Manages Redis connections
   - Provides methods for checking and storing idempotency state
   - Handles lock acquisition and release

2. **Idempotency Middleware** (`src/middleware/idempotency.rs`)
   - Axum middleware that wraps webhook handlers
   - Extracts idempotency key from headers
   - Coordinates request flow based on idempotency status

3. **Webhook Handler** (`src/handlers/webhook.rs`)
   - Business logic for processing webhooks
   - Protected by idempotency middleware

### Redis Key Structure
```
idempotency:{anchor_transaction_id} → "PROCESSING" | CachedResponse
```

## Testing

### Manual Testing

1. Start services:
```bash
docker-compose up -d
```

2. Send first request:
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

3. Send duplicate immediately (should get 429):
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

4. Wait a few seconds and send again (should get cached response):
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

### Verify Redis State
```bash
docker exec -it synapse-redis redis-cli
> KEYS idempotency:*
> GET idempotency:test-123
> TTL idempotency:test-123
```

## Error Handling

### Redis Connection Failure
- Middleware fails open (allows request to proceed)
- Logs error for monitoring
- Prevents Redis outage from blocking all webhooks

### Processing Timeout
- Processing lock expires after 5 minutes
- Allows retry if original request failed/hung
- Prevents permanent lock from crashed requests

## Security Considerations

1. **Key Validation**: Idempotency keys are validated for proper format
2. **TTL Limits**: Keys automatically expire to prevent Redis memory exhaustion
3. **Fail Open**: Redis failures don't block legitimate requests
4. **No Sensitive Data**: Only status codes and success flags stored in Redis

## Batch Semantics

Batch endpoints accept multiple items in one call and report a per-item outcome. The rules below apply uniformly to batch quote, batch order submission, bulk cancel, and bulk status.

### Ordering

- Results are returned in the same order as the submitted items; the response's `results` array is index-aligned with the request's `items` array.
- Each result carries the item's `index` so callers can correlate without relying on array position alone.
- Items are fanned out to the single-item code paths in-process with bounded concurrency, so execution order is not guaranteed — only the *reporting* order is. Do not depend on item N completing before item N+1.

### Atomicity

- Batches are **not** atomic. There is no all-or-nothing guarantee: some items may succeed while others fail.
- Each item is processed independently through the same single-item path, including all risk and compliance checks. Batch submission does not bypass or relax any per-item validation.
- A failure of one item never rolls back or prevents the processing of other items.

### Partial Failure Reporting

- The batch call itself returns `200 OK` (or `207 Multi-Status` where the spec uses it) whenever the batch was accepted and processed, even if some items failed. Per-item failures are reported in the body, not as a top-level error.
- Each item result has a `status` of `succeeded` or `failed`. Failed items include a machine-readable `error_code` and a human-readable `message`.
- A top-level error (non-2xx) is returned only for batch-level problems: malformed request, missing idempotency key, batch size cap exceeded, or quota exhausted before any item could be charged.
- One oversized or invalid item does **not** reject the entire batch; it fails on its own with its own error code. The whole batch is rejected only when the number of items exceeds the configured per-batch cap.

### Quota Accounting

- Quota is charged **per item**, not per batch call. A batch of N items consumes N units of the tenant's quota.
- Items that fail validation before consuming quota are not charged; items that reach the submission path are charged individually.
- If quota runs out mid-batch, the remaining items fail with a quota error code while already-processed items keep their outcomes. The batch is not rolled back.
- The per-batch size cap is enforced before fan-out, so an over-cap batch is rejected without charging any quota.

### Idempotency

- Batch retries are deduplicated at both the batch level and the per-item level; see section 6 above for the key derivation and its ordering caveat.

## Future Enhancements

1. **Response Body Caching**: Store full response body for exact replay
2. **Distributed Locking**: Use Redlock algorithm for multi-instance deployments
3. **Metrics**: Track duplicate request rates and cache hit ratios
4. **Configurable TTLs**: Make TTL values configurable per environment
