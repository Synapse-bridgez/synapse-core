# Swap Lookup Indexing and Query-Plan Strategy

Covers the read paths for `swap_orders`, `swap_quotes`, and `swap_fills`
(#1526). Partitioning and retention are covered in
[partitioning.md](partitioning.md); read-model projections and caching are
out of scope.

Source of truth:

| What | Where |
|------|-------|
| Query text and sample parameters | `src/swap/persistence/queries.rs` (`ACCESS_PATTERNS`) |
| Index definitions | `migrations/20261021000000_swap_lookup_indexes.sql` (`ensure_swap_lookup_indexes()`) |
| Regression guard | `swap_lookup_plans_use_indexes` test in `queries.rs` |

## Access patterns

All tables are assumed `PARTITION BY RANGE (created_at)` with a `tenant_id`
column and RLS by tenant.

| Pattern | Used by | Index |
|---------|---------|-------|
| Orders by tenant + status, time-bounded | order list API, stuck-order sweeps | `idx_swap_orders_tenant_status_created (tenant_id, status, created_at DESC, id DESC)` |
| Orders by tenant + time range | order list API, reconciliation | `idx_swap_orders_tenant_created (tenant_id, created_at DESC, id DESC)` |
| Orders by tenant + asset pair, time-bounded | per-pair reporting | `idx_swap_orders_tenant_pair_created (tenant_id, source_asset, target_asset, created_at DESC, id DESC)` |
| Order by idempotency key | create-order dedupe | `idx_swap_orders_tenant_idempotency (tenant_id, idempotency_key) WHERE idempotency_key IS NOT NULL` |
| Order by external reference | venue callbacks, support lookups | `idx_swap_orders_tenant_external_ref (tenant_id, external_ref) WHERE external_ref IS NOT NULL` |
| Quotes by tenant + asset pair, time-bounded | quote history | `idx_swap_quotes_tenant_pair_created` |
| Quotes by tenant + status, time-bounded | quote expiry sweeps | `idx_swap_quotes_tenant_status_created` |
| Fills by order | order detail, fill aggregation | `idx_swap_fills_tenant_order_created (tenant_id, order_id, created_at DESC, id DESC)` |
| Fills by tenant + time range | reconciliation jobs | `idx_swap_fills_tenant_created` |
| Fill by external reference | venue fill matching | `idx_swap_fills_tenant_external_ref (tenant_id, external_ref) WHERE external_ref IS NOT NULL` |

## Rules

1. **Lead with `tenant_id`.** Every query filters on it and every index starts
   with it, so RLS and the index agree and no tenant can force a cross-tenant scan.
2. **Bound `created_at` on both sides for list/range queries.** This is what
   lets the planner prune partitions; an unbounded range touches every
   partition's index. Point lookups by idempotency key / external reference
   are unbounded by design and probe each partition's (small, partial) index;
   callers that know the approximate time should add a `created_at` bound.
3. **Match the sort.** Indexes end in `created_at DESC, id DESC` so
   `ORDER BY created_at DESC, id DESC LIMIT n` is served by an index scan with
   no sort node, and keyset pagination on `(created_at, id)` stays cheap.
4. **No standalone low-cardinality indexes.** `status` and asset codes only
   appear after `tenant_id`; on their own they would rarely beat a seq scan
   and would add write amplification.
5. **Partial indexes for optional keys.** `idempotency_key` / `external_ref`
   are often `NULL`; partial indexes keep them small and limit bloat.

## Cost

Orders carry five indexes, quotes two, fills three. Each insert updates every
index on its partition, so before adding another index, show the pattern it
serves in `ACCESS_PATTERNS` and confirm an existing index cannot serve it
(a prefix of an existing composite index is already usable).

## Applying the indexes

The swap tables are created by separate work. `ensure_swap_lookup_indexes()`
is idempotent and skips missing tables; the migration that creates the swap
tables must call it after `CREATE TABLE`:

```sql
SELECT ensure_swap_lookup_indexes();
```

`CREATE INDEX` on a partitioned parent cascades to existing and future
partitions. Indexes added later to populated swap tables must follow
[hot-table-index-migrations.md](hot-table-index-migrations.md).

## Regression guard

`swap_lookup_plans_use_indexes` runs `EXPLAIN` for every pattern with
`enable_seqscan = off` and fails if a `Seq Scan` on the swap table remains -
meaning no index can serve the query. It needs a migrated database:

```bash
DATABASE_URL=postgres://... cargo test --lib swap_lookup_plans_use_indexes -- --ignored
```

Unit tests (no database) also assert every pattern is tenant-scoped, every
range pattern bounds `created_at`, and every referenced index is defined in
the migration.

For a manual check against production-like data volumes, inspect with
`EXPLAIN (ANALYZE, BUFFERS)` and confirm `Subplans Removed` / pruned
partitions appear and no `Sort` node sits above the index scan.
