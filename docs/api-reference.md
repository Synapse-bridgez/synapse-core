# API Reference

Base URL (local dev): `http://localhost:3000`

---

## OpenAPI Spec Drift Detection

The OpenAPI spec is generated at build time from `utoipa` route annotations across `src/handlers/` and is **not** committed to the repository (a committed generated file could itself drift). CI generates the spec as a build artifact and diffs it against a version-pinned baseline to catch SDK drift.

### How it works

1. CI runs the spec generator to emit `openapi.json` as a build artifact.
2. The generated spec is compared against the checked-in baseline at `sdks/rust/openapi-baseline.json`.
3. The diff is classified:
   - **Breaking changes** (removed paths/operations, removed or newly-required request fields, narrowed types, removed response fields) **fail the job hard**.
   - **Additive changes** (new paths, new optional fields, new response fields) **warn** and require an explicit acknowledgment checklist item in the PR before merge.

### Internal / admin routes

Routes under `/admin/*` and other intentionally-undocumented internal endpoints are excluded from the drift comparison via an allowlist so that internal route changes are not flagged as SDK-relevant drift. They are tracked separately and never gate the SDK drift check.

### Updating the baseline

When an intentional, SDK-reflected API change lands, regenerate the baseline:

```bash
cargo run --bin generate-openapi > sdks/rust/openapi-baseline.json
```

Commit the updated baseline alongside the corresponding SDK client changes in `sdks/rust/src/models.rs`.

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

Response `

/* … truncated 7063 chars — edit only what you need near the top … */
