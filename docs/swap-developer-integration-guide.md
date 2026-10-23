# Swap Engine Developer Integration Guide (#1478)

A narrative walkthrough for tenant developers integrating against the Phase 2 Synapse swap engine: lifecycle overview, authentication, environment setup, and a complete worked example of fetching a quote and placing an order.

## Table of Contents

1. [Lifecycle Overview](#lifecycle-overview)
2. [Authentication](#authentication)
3. [Environment Setup](#environment-setup)
4. [Worked Example: Quote → Order](#worked-example-quote--order)
5. [Order Status Polling](#order-status-polling)
6. [Cancelling an Order](#cancelling-an-order)
7. [Error Handling Reference](#error-handling-reference)
8. [Feature Flag — Swap Engine](#feature-flag--swap-engine)
9. [Links and Further Reading](#links-and-further-reading)

---

## Lifecycle Overview

The swap engine sits at the boundary between Phase 1 (fiat gateway) and Phase 2 (on-chain execution). Here is the full flow a deposit takes:

```
Fiat deposit webhook
      ↓
transactions table  (status: pending → processing → completed)
      ↓
swap_ready state    ← Phase 1 hands off here (swap_engine feature flag required)
      ↓
SwapHandoff         ← stable contract: transaction_id, source_asset, amount, ...
      ↓
Swap engine
   ├─ GET /swap/quotes  →  SwapQuote  (rate, fee, expiry, quote_id)
   ├─ POST /swap/orders →  SwapOrder  (order_id, status: pending)
   └─ Execution loop
         ├─ status: executing
         ├─ status: partially_filled (incremental fills)
         └─ status: filled   ← terminal success
                  or failed  ← terminal failure (see failure_code)
```

For the lifecycle state diagram see [STATE_MACHINE_DIAGRAMS.md](../STATE_MACHINE_DIAGRAMS.md).

### Status meanings

| Status | Meaning |
|---|---|
| `pending` | Order accepted; awaiting the execution worker |
| `executing` | Worker has taken a lease; fill in progress |
| `partially_filled` | At least one fill recorded; execution continues |
| `filled` | All source amount was exchanged; terminal success |
| `failed` | Terminal failure; see `failure_code` field |
| `cancelled` | Cancelled by tenant before execution; terminal |

Orders in `pending` or `partially_filled` may still be cancelled. Orders in `executing` or beyond cannot be cancelled (the LP may have already sent funds).

---

## Authentication

All swap endpoints require an API key tied to your tenant account. Pass it in the `X-API-Key` header:

```
X-API-Key: <your_api_key>
```

Tenant context is resolved server-side; you never pass a `tenant_id` in the body.

Rate limits apply per tenant per minute. The current limit is returned in the `X-RateLimit-Remaining` response header.

---

## Environment Setup

### Local development (Docker Compose)

```bash
# Clone the repo
git clone https://github.com/odunfemisamuel-maker/synapse-core.git
cd synapse-core

# Start the full stack including the swap profile
docker compose --profile swap up -d

# The swap engine is available at:
#   http://localhost:3001 (swap-app)
#   http://localhost:3000 (main app)

# Create a test tenant and get an API key
docker compose exec app cargo run --bin synapse-cli -- admin create-tenant \
  --name "My Dev Tenant"
```

### Environment variables

```env
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse
SWAP_ENGINE_ENABLED=true
PRICE_SOURCE_URL=http://localhost:3002   # swap-horizon-stub in Docker Compose
REDIS_URL=redis://localhost:6380         # swap-redis in Docker Compose
```

### Enabling the swap engine for your tenant

The swap engine is opt-in per tenant via the `swap_engine` feature flag:

```bash
# Via the admin CLI
cargo run --bin synapse-cli -- admin enable-feature \
  --tenant-id <your_tenant_id> \
  --flag swap_engine

# Or via the admin API
curl -X POST http://localhost:3000/admin/feature-flags \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"flag": "swap_engine", "tenant_id": "<your_tenant_id>", "enabled": true}'
```

---

## Worked Example: Quote → Order

This example swaps 100.50 USDC → XLM.

### Step 1: Request a quote

```bash
curl -s -X POST http://localhost:3001/swap/quotes \
  -H "X-API-Key: $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "source_asset": "USDC",
    "dest_asset": "XLM",
    "source_amount": "100.5"
  }'
```

Response `201`:

```json
{
  "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11",
  "source_asset": "USDC",
  "dest_asset": "XLM",
  "source_amount": "100.5",
  "dest_amount": "100.1985000",
  "rate": "0.997",
  "fee_bps": 30,
  "expires_at": "2026-10-24T12:00:30Z",
  "created_at": "2026-10-24T12:00:00Z"
}
```

Quotes expire after 60 seconds. If you place an order against an expired quote, you get `409 ERR_SWAP_001`.

### Step 2: Place an order against the quote

Place the order within the quote's `expires_at`. You must supply an `Idempotency-Key` header (1–128 characters matching `[A-Za-z0-9-_:.]`):

```bash
curl -s -X POST http://localhost:3001/swap/orders \
  -H "X-API-Key: $API_KEY" \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: my-order-$(date +%s)" \
  -d '{
    "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11"
  }'
```

Response `201`:

```json
{
  "order_id": "a3f1c2d4-1b2c-4d5e-8f60-71829a3b4c5d",
  "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11",
  "source_asset": "USDC",
  "dest_asset": "XLM",
  "source_amount": "100.5",
  "dest_amount": "100.1985000",
  "status": "pending",
  "created_at": "2026-10-24T12:00:05Z"
}
```

**Idempotency guarantee**: repeating the request with the same `Idempotency-Key` and payload returns the same `201` response. Repeating with a different payload returns `409 ERR_SWAP_003`.

---

## Order Status Polling

Poll `GET /swap/orders/{order_id}` to track progress:

```bash
curl -s http://localhost:3001/swap/orders/a3f1c2d4-1b2c-4d5e-8f60-71829a3b4c5d \
  -H "X-API-Key: $API_KEY"
```

Response:

```json
{
  "order_id": "a3f1c2d4-1b2c-4d5e-8f60-71829a3b4c5d",
  "status": "filled",
  "source_amount": "100.5",
  "filled_amount": "100.5",
  "dest_amount": "100.1985000",
  "created_at": "2026-10-24T12:00:05Z",
  "updated_at": "2026-10-24T12:00:12Z"
}
```

**Polling interval**: start at 1 second, back off to 5 seconds after 10 polls. Most orders fill within 5–15 seconds.

**Terminal statuses**: `filled`, `failed`, `cancelled`. Stop polling when one of these is reached.

### List your orders

```bash
curl -s "http://localhost:3001/swap/orders?limit=20&offset=0" \
  -H "X-API-Key: $API_KEY"
```

---

## Cancelling an Order

Only orders in `pending` status can be cancelled:

```bash
curl -s -X DELETE \
  http://localhost:3001/swap/orders/a3f1c2d4-1b2c-4d5e-8f60-71829a3b4c5d \
  -H "X-API-Key: $API_KEY"
```

Response `200` on success. `409 ERR_SWAP_002` if the order is no longer cancellable.

---

## Error Handling Reference

| HTTP | `code` | Meaning | What to do |
|---|---|---|---|
| 400 | `ERR_VALIDATION_001` | Missing/invalid field (asset, amount, bad `Idempotency-Key`) | Fix the request |
| 404 | `ERR_NOT_FOUND_001` | Quote or order not found (or belongs to another tenant) | Check the id |
| 409 | `ERR_SWAP_001` | Quote expired | Request a fresh quote |
| 409 | `ERR_SWAP_002` | Illegal status transition (cancel non-pending) | Check order status first |
| 409 | `ERR_SWAP_003` | Idempotency key reused with different payload | Change the key or match the payload |
| 422 | `ERR_SWAP_004` | Risk rejection (e.g., `amount_limit_exceeded`) | Check `reason_code` field; contact support if limits need raising |
| 429 | `ERR_RATE_LIMIT_001` | Rate limit exceeded | Back off; check `Retry-After` header |

All errors use the standard envelope:

```json
{
  "error": "Quote has expired",
  "code": "ERR_SWAP_001",
  "status": 409,
  "timestamp": "2026-10-24T12:01:00Z"
}
```

---

## Feature Flag — Swap Engine

The swap engine is disabled by default. Transactions for tenants without the flag set go through Phase 1 normally and stop at `completed` — they are not handed off and not backfilled.

To verify the flag is set for your tenant:

```bash
curl -s http://localhost:3000/admin/feature-flags/swap_engine?tenant_id=<your_tenant_id> \
  -H "Authorization: Bearer $ADMIN_TOKEN"
```

> **Behind a feature flag**: anything in this guide marked "⚠️ feature flag" is gated on `swap_engine` being enabled for your tenant. Calls made without the flag return a `404` as if the resource does not exist, not a permission error.

---

## Links and Further Reading

- [State Machine Diagrams](../STATE_MACHINE_DIAGRAMS.md) — transaction lifecycle and swap order transitions
- [Swap Operator Runbook](swap-operator-runbook.md) — how operators monitor and intervene
- [Swap DR Runbook](swap-dr-runbook.md) — backup, restore, and disaster recovery
- [Architecture Overview](architecture.md) — how the swap engine fits the broader system
- [Feature Flags](feature_flags.md) — how to enable and manage feature flags
