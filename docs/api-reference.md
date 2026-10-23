# API Reference

Base URL (local dev): `http://localhost:3000`

---

## Authentication

Most endpoints are unauthenticated. Endpoints under `/admin/*` require:

```
Authorization: Bearer <ADMIN_API_KEY>
```

`ADMIN_API_KEY` defaults to `admin-secret-key` in development. Set it via env var or Vault.

Webhook/callback endpoints authenticate via HMAC-SHA256 signature:

```
X-Stellar-Signature: <hex-encoded HMAC-SHA256 of request body>
```

---

## Rate Limiting

Callback and webhook endpoints are rate-limited per API key (or IP if no key is provided).

| Tier        | Limit (dev)    | Limit (prod)   |
|-------------|----------------|----------------|
| Default     | 10 000 req/min | 100 req/min    |
| Whitelisted | 100 000 req/min| 1 000 req/min  |

Rate limit headers are returned on every response:

```
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 99
X-RateLimit-Reset: 60
```

When exceeded, the server returns `429 Too Many Requests` with a `Retry-After` header.

---

## Health & Readiness

### `GET /health`

Returns service health including database connectivity and pool stats.

No authentication required.

```bash
curl http://localhost:3000/health
```

Response `200`:
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "db": "connected",
  "db_pool": {
    "active_connections": 3,
    "idle_connections": 7,
    "max_connections": 50,
    "usage_percent": 6.0
  },
  "pending_queue_depth": 0,
  "current_batch_size": 10
}
```

Response `503` when database is unreachable — same body with `"status": "unhealthy"`.

---

### `GET /ready`

Kubernetes readiness probe. Returns `503` during connection draining or before initialization completes.

No authentication required.

```bash
curl http://localhost:3000/ready
```

Response `200`:
```json
{ "status": "ready", "draining": false }
```

Response `503`:
```json
{ "status": "not_ready", "draining": true }
```

---

### `GET /errors`

Returns the full error code catalog.

No authentication required.

```bash
curl http://localhost:3000/errors
```

Response `200`:
```json
{
  "errors": [
    { "code": "VALIDATION_ERROR", "description": "..." }
  ],
  "version": "1.0.0"
}
```

---

## Transactions

### `POST /callback`

Receive a Stellar Anchor Platform webhook and create a transaction.

Rate-limited. Requires `X-Stellar-Signature` header for HMAC verification.

```bash
curl -X POST http://localhost:3000/callback \
  -H "Content-Type: application/json" \
  -H "X-Stellar-Signature: <hmac-sha256-hex>" \
  -d '{
    "stellar_account": "GAAZI4TCR3TY5OJHCTJC2A4QM7S4WXZ3XQFTKJBBHKS3HZXBCXQXQXQX",
    "amount": "100.00",
    "asset_code": "USDC",
    "callback_type": "deposit",
    "callback_status": "completed",
    "anchor_transaction_id": "anchor-tx-001",
    "memo": "payment ref",
    "memo_type": "text"
  }'
```

Request body:

| Field                  | Type   | Required | Description                              |
|------------------------|--------|----------|------------------------------------------|
| stellar_account        | string | yes      | Stellar public key (G...)                |
| amount                 | string | yes      | Positive decimal amount                  |
| asset_code             | string | yes      | Uppercase asset code (e.g. USDC)         |
| callback_type          | string | no       | e.g. `deposit`, `withdrawal`             |
| callback_status        | string | no       | e.g. `completed`, `pending`              |
| anchor_transaction_id  | string | no       | Anchor-side transaction ID (max 255)     |
| memo                   | string | no       | Transaction memo                         |
| memo_type              | string | no       | `text`, `hash`, or `id`                  |
| metadata               | object | no       | Arbitrary JSON metadata                  |

Response `201`:
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "stellar_account": "GAAZI4TCR3TY5OJHCTJC2A4QM7S4WXZ3XQFTKJBBHKS3HZXBCXQXQXQX",
  "amount": "100.00",
  "asset_code": "USDC",
  "status": "pending",
  "created_at": "2026-04-25T12:00:00Z"
}
```

Response `400` — validation error:
```json
{ "error": "stellar_account: invalid Stellar address" }
```

Response `503` — back-pressure (queue full):
```json
{ "error": "service busy, retry later" }
```

---

### `POST /callback/transaction`

Alias for `POST /callback`. Identical behaviour.

```bash
curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -H "X-Stellar-Signature: <hmac-sha256-hex>" \
  -d '{ "stellar_account": "G...", "amount": "50.00", "asset_code": "XLM" }'
```

---

### `POST /webhook`

Generic webhook ingestion endpoint. Accepts a payload with an `id` field and acknowledges it.

Rate-limited. Requires `X-Stellar-Signature` header.

```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Stellar-Signature: <hmac-sha256-hex>" \
  -d '{ "id": "evt-12345" }'
```

Request body:

| Field | Type   | Required | Description      |
|-------|--------|----------|------------------|
| id    | string | yes      | Webhook event ID |

Response `200`:
```json
{ "success": true, "message": "Webhook evt-12345 processed successfully" }
```

---

### `GET /transactions`

List transactions with cursor-based pagination.

No authentication required.

```bash
curl "http://localhost:3000/transactions?limit=25"

# Next page
curl "http://localhost:3000/transactions?cursor=<next_cursor>&limit=25"

# Date range filter
curl "http://localhost:3000/transactions?from_date=2026-01-01T00:00:00Z&to_date=2026-02-01T00:00:00Z"
```

Query parameters:

| Parameter  | Type   | Default | Description                                  |
|------------|--------|---------|----------------------------------------------|
| cursor     | string | —       | Opaque pagination cursor from previous page  |
| limit      | int    | 25      | Page size (max 100)                          |
| direction  | string | forward | `forward` or `backward`                      |
| from_date  | string | —       | ISO 8601 start date (inclusive)              |
| to_date    | string | —       | ISO 8601 end date (exclusive)                |

Response `200`:
```json
{
  "data": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "stellar_account": "G...",
      "amount": "100.00",
      "asset_code": "USDC",
      "status": "completed",
      "created_at": "2026-04-25T12:00:00Z",
      "updated_at": "2026-04-25T12:01:00Z"
    }
  ],
  "meta": {
    "next_cursor": "eyJ0cyI6...",
    "has_more": true
  }
}
```

When reading from a replica, the response includes:
```
X-Read-Consistency: eventual
```

---

### `GET /transactions/:id`

Get a single transaction by UUID.

No authentication required.

```bash
curl http://localhost:3000/transactions/550e8400-e29b-41d4-a716-446655440000
```

Response `200` — transaction object (same shape as list items above).

Response `404`:
```json
{ "error": "Transaction 550e8400-... not found" }
```

---

### `GET /transactions/search`

Search transactions with filters.

No authentication required.

```bash
curl "http://localhost:3000/transactions/search?status=completed&asset_code=USDC&min_amount=10&max_amount=1000"
```

Query parameters:

| Parameter      | Type   | Description                          |
|----------------|--------|--------------------------------------|
| status         | string | Filter by status                     |
| asset_code     | string | Filter by asset code                 |
| min_amount     | string | Minimum amount (decimal)             |
| max_amount     | string | Maximum amount (decimal)             |
| from_date      | string | ISO 8601 start date                  |
| to_date        | string | ISO 8601 end date                    |
| stellar_account| string | Filter by Stellar account            |
| cursor         | string | Pagination cursor                    |
| limit          | int    | Page size (max 100, default 25)      |

Response `200`:
```json
{
  "total": 42,
  "data": [ ... ]
}
```

---

### `GET /export`

Export transactions as CSV or JSON (streaming).

No authentication required.

```bash
# CSV (default)
curl "http://localhost:3000/export?format=csv&from=2026-01-01&to=2026-04-30" \
  -o transactions.csv

# JSON Lines
curl "http://localhost:3000/export?format=json&status=completed" \
  -o transactions.json
```

Query parameters:

| Parameter  | Type   | Default | Description                          |
|------------|--------|---------|--------------------------------------|
| format     | string | csv     | `csv` or `json`                      |
| from       | string | —       | Start date `YYYY-MM-DD`              |
| to         | string | —       | End date `YYYY-MM-DD` (inclusive)    |
| status     | string | —       | Filter by status                     |
| asset_code | string | —       | Filter by asset code                 |

Response `200` with `Content-Disposition: attachment; filename="transactions_YYYY-MM.csv"`.

---

## Settlements

### `GET /settlements`

List settlements with cursor-based pagination.

No authentication required.

```bash
curl "http://localhost:3000/settlements?limit=10"
```

Query parameters:

| Parameter  | Type   | Default | Description                         |
|------------|--------|---------|-------------------------------------|
| cursor     | string | —       | Pagination cursor                   |
| limit      | int    | 10      | Page size (max 100, min 1)          |
| direction  | string | forward | `forward` or `backward`             |

Response `200`:
```json
{
  "settlements": [
    {
      "id": "...",
      "amount": "5000.00",
      "asset_code": "USDC",
      "status": "completed",
      "created_at": "2026-04-25T00:00:00Z"
    }
  ],
  "next_cursor": "eyJ0cyI6...",
  "has_more": false
}
```

---

### `GET /settlements/:id`

Get a single settlement by UUID.

No authentication required.

```bash
curl http://localhost:3000/settlements/550e8400-e29b-41d4-a716-446655440000
```

Response `200` — settlement object.

Response `404` when not found.

---

## Statistics

### `GET /stats/status`

Transaction counts grouped by status. Results are cached in Redis.

No authentication required.

```bash
curl http://localhost:3000/stats/status
```

Response `200`:
```json
[
  { "status": "pending", "count": 12 },
  { "status": "completed", "count": 980 },
  { "status": "failed", "count": 8 }
]
```

---

### `GET /stats/daily`

Daily transaction totals for the last N days.

No authentication required.

```bash
curl "http://localhost:3000/stats/daily?days=7"
```

Query parameters:

| Parameter | Type | Default | Description          |
|-----------|------|---------|----------------------|
| days      | int  | 7       | Number of days back  |

Response `200`:
```json
[
  { "date": "2026-04-25", "count": 42, "total_amount": "4200.00" }
]
```

---

### `GET /stats/assets`

Transaction stats grouped by asset code.

No authentication required.

```bash
curl http://localhost:3000/stats/assets
```

Response `200`:
```json
[
  { "asset_code": "USDC", "count": 500, "total_amount": "50000.00" }
]
```

---

### `GET /cache/metrics`

Cache hit/miss metrics for query cache and idempotency cache.

No authentication required.

```bash
curl http://localhost:3000/cache/metrics
```

Response `200`:
```json
{
  "query_cache": { "hits": 120, "misses": 30 },
  "idempotency_cache_hits": 45,
  "idempotency_cache_misses": 5,
  "idempotency_lock_acquired": 50,
  "idempotency_lock_contention": 2,
  "idempotency_errors": 0,
  "idempotency_fallback_count": 1
}
```

---

## GraphQL

### `POST /graphql`

GraphQL endpoint. Supports queries for transactions and settlements.

No authentication required.

```bash
curl -X POST http://localhost:3000/graphql \
  -H "Content-Type: application/json" \
  -d '{
    "query": "{ transaction(id: \"550e8400-e29b-41d4-a716-446655440000\") { id status amount } }"
  }'
```

Response `200`:
```json
{
  "data": {
    "transaction": {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "status": "completed",
      "amount": "100.00"
    }
  }
}
```

---

## Admin

All admin endpoints require `Authorization: Bearer <ADMIN_API_KEY>`.

### `PATCH /admin/transactions/bulk-status`

Bulk update transaction statuses (max 500 per request).

```bash
curl -X PATCH http://localhost:3000/admin/transactions/bulk-status \
  -H "Authorization: Bearer dev-admin-key" \
  -H "Content-Type: application/json" \
  -d '{
    "transaction_ids": [
      "550e8400-e29b-41d4-a716-446655440000",
      "660e8400-e29b-41d4-a716-446655440001"
    ],
    "status": "failed",
    "reason": "manual override"
  }'
```

Request body:

| Field           | Type     | Required | Description                                          |
|-----------------|----------|----------|------------------------------------------------------|
| transaction_ids | string[] | yes      | UUIDs to update (1–500)                              |
| status          | string   | yes      | `pending`, `processing`, `completed`, or `failed`    |
| reason          | string   | no       | Audit reason                                         |

Response `200`:
```json
{
  "updated": 2,
  "failed": 0,
  "errors": []
}
```

---

### `POST /admin/drain`

Kubernetes preStop hook. Marks the service as not-ready and starts the drain timer. The process exits after the drain timeout (default 30 s).

```bash
curl -X POST http://localhost:3000/admin/drain \
  -H "Authorization: Bearer dev-admin-key"
```

Response `200`:
```json
{ "status": "draining", "drain_timeout_secs": 30 }
```

See [deployment.md](deployment.md) for the full Kubernetes setup.

---

### `GET /admin/webhooks/health`

List health scores for all webhook endpoints.

```bash
curl http://localhost:3000/admin/webhooks/health \
  -H "Authorization: Bearer dev-admin-key"
```

Response `200`:
```json
[
  {
    "endpoint_id": "...",
    "url": "https://example.com/hook",
    "health_score": 0.95,
    "consecutive_failures": 0,
    "last_delivery_at": "2026-04-25T12:00:00Z"
  }
]
```

---

### `GET /admin/webhooks/health/:id`

Get health score for a specific webhook endpoint.

```bash
curl http://localhost:3000/admin/webhooks/health/550e8400-e29b-41d4-a716-446655440000 \
  -H "Authorization: Bearer dev-admin-key"
```

Response `200` — single health object (same shape as list item above).

Response `404` when endpoint not found.

---

## Error Codes

| HTTP Status | Meaning                                                  |
|-------------|----------------------------------------------------------|
| 400         | Bad request — invalid input or missing required fields   |
| 401         | Unauthorized — missing or invalid auth header            |
| 404         | Not found                                                |
| 429         | Too many requests — rate limit exceeded                  |
| 500         | Internal server error                                    |
| 503         | Service unavailable — draining, not ready, or queue full |
