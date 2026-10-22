# Swap API Reference

Endpoints for requesting swap quotes and managing swap orders (#1440).

## Conventions

- **Base paths / versioning:** served under `/api/v1` and `/api/v2` (same
  handlers, versioning headers from the existing middleware). Within a version
  changes are additive only: new optional fields and new endpoints, never
  renamed or removed fields.
- **Auth / tenancy:** the caller's tenant is resolved by the `TenantContext`
  extractor (API key / tenant headers, active-tenant and rate-limit checks).
  Every swap query is filtered by that tenant at the persistence layer; another
  tenant's quote or order is indistinguishable from a missing one (404).
- **Amounts** are decimal strings with at most 7 fractional digits.
- **Assets** are `CODE` (1-12 alphanumerics) or `CODE:ISSUER` (issuer is a
  56-character `G...` Stellar account).
- **Order status** (`pending`, `executing`, `completed`, `failed`, `canceled`)
  is server-controlled output only; clients can filter by it but never set it.
- **Body limit:** swap endpoints reject request bodies over 4 KiB.
- **Responses never contain** tenant ids, idempotency keys, request hashes,
  stack traces or risk-policy thresholds.

## Error contract

Errors use the standard envelope (`error`, `code`, `status`, `timestamp`,
`detail`, `docs_url`).

| Condition | HTTP | `code` |
|---|---|---|
| Validation (assets, amount, missing/invalid `Idempotency-Key`, bad filter) | 400 | `ERR_VALIDATION_001` |
| Unknown quote/order (or owned by another tenant) | 404 | `ERR_NOT_FOUND_001` |
| Quote expired | 409 | `ERR_SWAP_001` |
| Illegal transition (e.g. cancel a non-`pending` order, quote already used) | 409 | `ERR_SWAP_002` |
| Idempotency key reused with a different payload | 409 | `ERR_SWAP_003` |
| Risk rejection | 422 | `ERR_SWAP_004` plus a `reason_code` field (e.g. `amount_limit_exceeded`) |
| Rate limit | 429 | `ERR_RATE_LIMIT_001` |

## `POST /swap/quotes`

```json
{ "source_asset": "USDC", "dest_asset": "XLM", "source_amount": "100.5" }
```

`201`:

```json
{
  "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11",
  "source_asset": "USDC",
  "dest_asset": "XLM",
  "source_amount": "100.5",
  "dest_amount": "100.1985000",
  "rate": "0.997",
  "fee_bps": 30,
  "expires_at": "2026-10-19T12:00:30Z",
  "created_at": "2026-10-19T12:00:00Z"
}
```

## `POST /swap/orders`

Requires header `Idempotency-Key: <1-128 chars of [A-Za-z0-9-_:.]>`.

```json
{ "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11" }
```

`201`:

```json
{
  "order_id": "a3f1c2d4-1b2c-4d5e-8f60-71829a3b4c5d",
  "quote_id": "5c1b0f6e-6d0e-4f0a-9d53-0b8f1d2f7a11",
  "source_asset": "USDC",
  "dest_asset": "XLM",
  "source_amount": "100.5",
  "dest_amount": "100.1985000",
  "status": "pending",
  "created_at": "2026-10-19T12:00:05Z",
  "updated_at": "2026-10-19T12:00:05Z"
}
```

Replaying the same key with the same `quote_id` returns the originally created
order (same body, even if the quote has since expired). The same key with a
different `quote_id` returns `409 ERR_SWAP_003`.

Risk rejection example (`422`):

```json
{ "error": "Swap rejected by risk policy: amount_limit_exceeded", "code": "ERR_SWAP_004", "status": 422, "reason_code": "amount_limit_exceeded" }
```

## `GET /swap/orders/{id}`

`200` with the order object above, or `404`.

## `GET /swap/orders`

Query: `status`, `created_from` (inclusive, RFC 3339), `created_to` (exclusive),
`cursor`, `limit` (1-100, default 20). Newest first.

```json
{ "orders": [ { "order_id": "...", "status": "pending" } ], "next_cursor": "MjAyNi0...", "has_more": true }
```

Pagination is keyset-based on `(created_at, id)` and served by the
`(tenant_id, created_at DESC, id DESC)` indexes, so deep pages never use
`OFFSET` or a sequential scan. Pass `next_cursor` back as `cursor`; it is `null`
on the last page.

## `POST /swap/orders/{id}/cancel`

Cancels a `pending` order and returns it with `status: "canceled"`. Any other
status returns `409 ERR_SWAP_002`.
