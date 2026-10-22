# Database Partitioning Implementation

## Overview

The `transactions` table has been converted to a partitioned table using PostgreSQL's native range partitioning by `created_at` timestamp. This enables efficient handling of millions of records by:

- Improving query performance on time-based queries
- Faster VACUUM and maintenance operations
- Easy archival of old data by detaching partitions
- Automatic partition management

## Architecture

### Partitioning Strategy
- **Type**: Range partitioning
- **Key**: `created_at` (TIMESTAMPTZ)
- **Interval**: Monthly partitions
- **Naming**: `transactions_y{YYYY}m{MM}` (e.g., `transactions_y2025m02`)

### Key Changes
1. Primary key changed to composite: `(id, created_at)`
2. Automatic partition creation for upcoming months
3. Retention policy to detach partitions older than 12 months
4. Background cron job runs every 24 hours

### Swap Orders
The `swap_orders` table (`migrations/20261019000000_swap_orders.sql`) follows the
same strategy: monthly range partitioning by `created_at`, composite primary key
`(id, created_at)`, and partitions named `swap_orders_y{YYYY}m{MM}`. Initial
partitions cover 2026-10 through 2026-12. The scheduled partition maintenance
currently only extends `transactions`, so `swap_orders` partitions must be
added before they are needed.

## Database Functions

### `create_monthly_partition()`
Creates a partition for 2 months ahead if it doesn't exist.

```sql
SELECT create_monthly_partition();
```

### `detach_old_partitions(retention_months INTEGER)`
Detaches partitions older than the specified retention period (default: 12 months).

```sql
SELECT detach_old_partitions(12);
```

### `maintain_partitions()`
Combines both operations - creates new partitions and detaches old ones.

```sql
SELECT maintain_partitions();
```

## Partition Manager

The `PartitionManager` runs as a background task in the application:

```rust
use synapse_core::db::partition::PartitionManager;

// Runs maintenance every 24 hours
let manager = PartitionManager::new(pool.clone(), 24);
manager.start();
```

### Manual Operations

```rust
// Create partition manually
manager.create_partition().await?;

// Detach old partitions with custom retention
manager.detach_old_partitions(6).await?; // Keep 6 months
```

## Swap Tables

The Phase 2 swap tables use the same monthly `created_at` range partitioning as `transactions`
(migration `20260928000000_swap_partitioned_tables.sql`):

| Table | Purpose | Partition key | Primary key |
|-------|---------|---------------|-------------|
| `swap_orders` | One row per swap order | `created_at` | `(id, created_at)` |
| `swap_quotes` | Quotes issued for an order | `created_at` | `(id, created_at)` |
| `swap_fills` | Executions against a quote | `created_at` | `(id, created_at)` |
| `swap_events` | Transactional outbox | `created_at` | `(id, created_at)` |
| `swap_order_idempotency` | Global idempotency authority (not partitioned) | — | `(tenant_id, idempotency_key)` |

Partitions are named `<table>_yYYYYmMM`, e.g. `swap_orders_y2026m10`.

### Uniqueness and idempotency

A partitioned table's unique constraints must include the partition key, so `swap_orders`
cannot enforce a global unique order id or idempotency key by itself. Idempotency is enforced
by `swap_order_idempotency`, which is not partitioned. Create the order and its idempotency row
in one transaction:

```sql
INSERT INTO swap_order_idempotency (tenant_id, idempotency_key, order_id)
VALUES ($1, $2, $3);   -- fails with unique_violation on replay
```

### Retention

`detach_swap_partitions(retention_months)` runs from `maintain_partitions()` with the
configured `SWAP_RETENTION_MONTHS` (default `12`). For each swap table it detaches partitions whose
month starts before the cutoff, with two exceptions:

- A partition is **kept** while any non-terminal order (`status NOT IN ('completed', 'failed', 'cancelled')`)
  was created before the partition's end. Its quotes and fills may still be in the partition or
  a later one.
- A `swap_events` partition is **kept** while it holds unpublished outbox rows (`published_at IS NULL`).

Detaching `swap_orders` also deletes `swap_order_idempotency` rows created before the partition's
end, so the idempotency window matches the retention window.

Detached partitions are standalone tables. Archive them as described under
[Archival Strategy](#archival-strategy) and drop them once archived.

### Manual operations

```sql
-- Create partitions for the current month plus the next two, across all swap tables
SELECT ensure_swap_partitions(3);

-- Create one month's swap partitions (e.g. for a backfill)
SELECT ensure_swap_partition_for('2027-01-15');

-- Detach swap partitions older than 6 months (still subject to the non-terminal guard)
SELECT detach_swap_partitions(6);
```

`ensure_swap_partitions()` and `detach_swap_partitions()` are idempotent. Re-running them is safe.

## Migration Process

The migration (`20250217000000_partition_transactions.sql`) performs:

1. Renames `transactions` → `transactions_old`
2. Creates new partitioned `transactions` table
3. Creates initial 3 monthly partitions
4. Migrates existing data
5. Sets up maintenance functions

### Rollback (if needed)

```sql
-- Restore old table
DROP TABLE IF EXISTS transactions;
ALTER TABLE transactions_old RENAME TO transactions;
```

## Performance Considerations

### Query Optimization
Queries filtering by `created_at` will automatically use partition pruning:

```sql
-- Only scans relevant partition(s)
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' 
  AND created_at < '2025-03-01';
```

### Indexes
Each partition inherits indexes from the parent table:
- `idx_transactions_status`
- `idx_transactions_stellar_account`
- `idx_transactions_created_at`

## Monitoring

### Check Existing Partitions

```sql
SELECT 
    c.relname AS partition_name,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS size
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname;
```

### Check Partition Bounds

```sql
SELECT 
    c.relname AS partition_name,
    pg_get_expr(c.relpartbound, c.oid) AS partition_bound
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions';
```

## Archival Strategy

Detached partitions remain as regular tables and can be:

1. **Archived to cold storage**:
```sql
-- Export to file
COPY transactions_y2024m01 TO '/archive/transactions_2024_01.csv' CSV HEADER;

-- Drop after backup
DROP TABLE transactions_y2024m01;
```

2. **Moved to archive schema**:
```sql
CREATE SCHEMA IF NOT EXISTS archive;
ALTER TABLE transactions_y2024m01 SET SCHEMA archive;
```

3. **Compressed**:
```sql
-- Using pg_squeeze or similar tools
```

## Swap Domain Tables

The Phase 2 swap tables follow the same monthly range-partitioning pattern
(migration `20260928000001_swap_tables.sql`):

| Table | Purpose | Partition key | Primary key | Foreign key |
|-------|---------|---------------|-------------|-------------|
| `swap_quotes` | Quotes issued (immutable) | `created_at` | `(id, created_at)` | — |
| `swap_orders` | Orders executed against a quote | `created_at` | `(id, created_at)` | `(quote_id, quote_created_at)` → `swap_quotes` |
| `swap_fills` | Execution fills against an order (immutable) | `created_at` | `(id, created_at)` | `(order_id, order_created_at)` → `swap_orders` |
| `swap_events` | Append-only order lifecycle log | `created_at` | `(id, created_at)` | `(order_id, order_created_at)` → `swap_orders` |

Partition names follow the same convention: `swap_quotes_yYYYYmMM`,
`swap_orders_yYYYYmMM`, `swap_fills_yYYYYmMM`, `swap_events_yYYYYmMM`.

### Why the foreign keys carry `created_at`

PostgreSQL requires a partitioned table's unique constraints to include the
partition key. A foreign key therefore references `(id, created_at)`, and the
child row stores the parent's `created_at` next to its `id`
(`quote_created_at`, `order_created_at`).

### Monetary amounts and statuses

- All amounts are `NUMERIC`, never floating point.
- `swap_orders.status`, `swap_events.from_status`, and `swap_events.to_status`
  are `CHECK`-constrained to: `pending`, `routed`, `partially_filled`, `filled`,
  `failed`, `cancelled`. Transition rules are enforced by the swap engine.

### Optimistic concurrency

`update_swap_order_status` (in `src/db/swap_queries.rs`) runs
`UPDATE ... WHERE id = $1 AND created_at = $2 AND status = $3`. It returns
`None` when another writer has already moved the order, so the caller reloads
instead of overwriting.

### Append-only events

`swap_events` has a `BEFORE UPDATE OR DELETE` row trigger
(`swap_events_reject_mutation()`) that raises an exception. The trigger is
cloned onto partitions created after the migration.

### Partition management

The swap tables reuse the shared routines instead of a second implementation:

- `ensure_partition_for_table(parent_table, date)`: advisory-locked, idempotent
  creation. `ensure_partition_for(date)` now delegates to it for `transactions`.
- `detach_old_partitions_for_table(parent_table, months)`: retention detach.
  `detach_old_partitions(months)` now delegates to it for `transactions`.
- `ensure_swap_partitions(months_ahead)`: applies the first routine to all four
  swap tables. `PartitionManager::maintain_partitions` calls it with the same
  lookahead as `transactions`.
- `detach_old_swap_partitions(retention_months)`: applies the retention routine
  to all four swap tables.

### Retention decision for swap tables

- **Retention: 24 months**, applied by `detach_old_swap_partitions(24)`. Swap
  quotes, orders, fills, and events are audit records, so they are kept longer
  than the 12-month default for `transactions`.
- **Not run automatically.** `maintain_partitions` only creates partitions
  ahead. Detaching is run manually (or from a scheduled job) so that archiving
  can be reviewed first.
- Detached partitions stay regular tables and follow the archival steps above.

### Insert outside existing partitions

Monthly partitions are created ahead of time, so an insert whose `created_at`
falls outside every existing partition fails with SQLSTATE 23514. The
`transactions` path self-heals this case in Rust. The swap queries do not:
they rely on the lookahead window. If a swap insert fails with 23514, run
`SELECT ensure_swap_partitions(n);` and retry.

## Testing

Run tests with partitioned table:

```bash
DATABASE_URL=postgres://synapse:synapse@localhost:5432/synapse_test cargo test
```

## Requirements

- PostgreSQL 14+ (native declarative partitioning)
- No external dependencies (pg_partman not required)

## Future Enhancements

- [ ] Automatic compression of old partitions
- [ ] Metrics/alerts for partition health
- [ ] Dynamic retention policy based on storage
- [ ] Sub-partitioning by status or account (if needed)
