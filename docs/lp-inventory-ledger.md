# Liquidity Provider Inventory Ledger & Treasury Rebalancing (#1461)

Status: **design note / not yet implemented.** This document specifies the
inventory ledger model, the reserve/release lifecycle, the snapshot API, the
threshold-breach events and the rebalancing recommendations. It is the
implementation plan for #1461; no code from this plan has landed yet.

## Problem

Swap orders are accepted without any record of which liquidity provider (LP)
is backing them. There is no per-tenant, per-asset, per-LP balance, so:

- the same inventory can be promised to more than one order (over-commitment),
- operators cannot see how much of an asset is actually free to trade,
- nothing warns when an asset runs dry, and
- nothing suggests where to move treasury funds when balances drift.

## Ledger model

One row per `(tenant_id, asset_code, lp_id)` in a new table
`lp_inventory_balances`:

| column        | type           | notes                                        |
| ------------- | -------------- | -------------------------------------------- |
| `tenant_id`   | `UUID`         | FK `tenants(tenant_id)`                      |
| `asset_code`  | `TEXT`         | FK `assets(asset_code)`                      |
| `lp_id`       | `TEXT`         | LP identifier (Stellar account or LP key)    |
| `balance`     | `NUMERIC(38,18)` | total units held                           |
| `reserved`    | `NUMERIC(38,18)` | units held against open orders             |
| `min_available` | `NUMERIC(38,18)` | threshold for breach events (default 0)   |
| `target_share`  | `NUMERIC(9,6)`   | desired share of tenant asset total (0..1) |
| `updated_at`  | `TIMESTAMPTZ`  | `NOW()` on every write                       |

Primary key `(tenant_id, asset_code, lp_id)`.

**Available is never stored.** It is always derived:

```
available = balance - reserved
```

This is exposed as a generated column (`GENERATED ALWAYS AS (balance - reserved) STORED`)
so SQL readers get it for free, and as an explicit field on every API response.
Invariant enforced by a `CHECK (reserved >= 0 AND reserved <= balance)`.

A second append-only table `lp_inventory_ledger_entries` records every
mutation for auditability:

| column        | type           | notes                                              |
| ------------- | -------------- | -------------------------------------------------- |
| `id`          | `BIGSERIAL`    | PK                                                 |
| `tenant_id`, `asset_code`, `lp_id` | | same key as the balance row                 |
| `order_id`    | `UUID`         | swap order the entry belongs to                    |
| `kind`        | `TEXT`         | `reserve` \| `release` \| `fill` \| `adjustment`   |
| `amount`      | `NUMERIC(38,18)` | always positive; `kind` gives the sign           |
| `reason`      | `TEXT`         | `accepted` \| `filled` \| `cancelled` \| `expired` \| `rollback` |
| `created_at`  | `TIMESTAMPTZ`  | `NOW()`                                            |

Unique index on `(order_id, kind, reason)` makes reserve/release idempotent:
a replayed lifecycle event is a no-op instead of double-counting.

## Reserve / release lifecycle

| swap order event | ledger effect                                                        |
| ---------------- | -------------------------------------------------------------------- |
| accepted         | `reserved += amount` (fails with `InsufficientInventory` if `available < amount`) |
| filled           | `balance -= amount`, `reserved -= amount` (inventory leaves the LP)   |
| cancelled        | `reserved -= amount` (inventory returns to available)                 |
| expired          | `reserved -= amount`                                                  |
| rollback         | `reserved -= amount` (compensating entry for a failed fill)           |

All mutations run in one transaction with `SELECT ... FOR UPDATE` on the
balance row, so concurrent orders cannot both consume the same units.

## Snapshot API

`GET /inventory/snapshot` (tenant-scoped, `TenantContext` extractor, same
pattern as `/settlements`):

```json
{
  "tenant_id": "…",
  "generated_at": "…",
  "balances": [
    { "asset_code": "USD", "lp_id": "G…", "balance": "1000",
      "reserved": "250", "available": "750",
      "min_available": "100", "target_share": "0.5",
      "below_minimum": false }
  ],
  "breaches": [ { "asset_code": "EUR", "lp_id": "G…", "available": "10",
                  "min_available": "50" } ],
  "recommendations": [ { "asset_code": "USD", "from_lp": "G…", "to_lp": "G…",
                         "amount": "120", "reason": "drift_above_target" } ]
}
```

Optional query params: `asset_code`, `lp_id`.

## Threshold-breach events

When a mutation leaves `available < min_available`, emit
`InventoryThresholdBreach { tenant_id, asset_code, lp_id, available, min_available }`
on a `tokio::sync::broadcast` channel held in `AppState` (mirroring
`tx_broadcast`), and increment a counter metric
`inventory_threshold_breaches_total{asset_code}`. Emission happens after the
DB transaction commits so a rolled-back mutation never emits.

## Rebalancing recommendations

For each `(tenant_id, asset_code)`, compute each LP's actual share
`available / Σ available`. If `|actual - target_share| > drift_tolerance`
(default `0.05`, configurable per tenant), recommend moving
`(target_share - actual) * Σ available` units from the over-weighted LP to the
under-weighted one. Recommendations are advisory only — nothing moves funds.

## Files to change

1. `migrations/20261101000000_lp_inventory_ledger.sql` (+ `.down.sql`) — both
   tables, generated `available` column, checks, indexes.
2. `src/services/inventory.rs` — `InventoryService` with
   `reserve`, `release`, `fill`, `snapshot`, `recommendations`; unit tests for
   the `available = balance - reserved` invariant and idempotent replay.
3. `src/services/mod.rs` — `pub mod inventory;` + re-export.
4. `src/handlers/inventory.rs` — `GET /inventory/snapshot` handler.
5. `src/handlers/mod.rs` — `pub mod inventory;`.
6. `src/lib.rs` — mount the route on `data_routes`; add
   `inventory_events: broadcast::Sender<InventoryThresholdBreach>` to
   `AppState` (and to `main.rs` / `test_new`).
7. `src/swap/mod.rs` (or the swap order lifecycle owner) — call
   `reserve` on acceptance and `release`/`fill` on terminal events.
8. `docs/api-reference.md` — document the snapshot endpoint.

## Steps in order

1. Migration + down migration; run `./scripts/check-migration-safety.sh`.
2. `InventoryService` with reserve/release/fill and the ledger-entry
   idempotency guard; tests.
3. Snapshot query + recommendations; tests.
4. Handler + route + `AppState` broadcast channel.
5. Wire the swap order lifecycle events to the service.
6. Docs.

## Done when

- `available = balance - reserved` holds after every operation and is exposed
  by the snapshot API.
- Reserve on acceptance; release on fill, cancel, expiry and rollback; replaying
  any event does not change balances.
- `GET /inventory/snapshot` returns balances, breaches and recommendations for
  the calling tenant only.
- A breach event is emitted exactly once per crossing below `min_available`.
- `cargo fmt --all -- --check`, `cargo clippy -- -D warnings`, `cargo build`
  and `cargo test` pass.
