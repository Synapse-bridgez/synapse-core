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

## Expand/Contract Migration Policy

All schema changes to swap engine tables (orders, quotes, fills, events, screenings, submissions) and to the partitioned `transactions` table MUST follow the expand/contract practice described below. Naive migrations that drop or rename columns break running replicas and block zero-downtime deploys, so they are not permitted outside the contract phase.

### Phases

1. **Expand** — ship an additive, backward-compatible migration:
   - Add new columns as `NULL`-able, or `NOT NULL` **with** a `DEFAULT` (or a `DEFAULT` added in the same statement).
   - Add new tables, indexes, and constraints as separate statements.
   - Never drop or rename a column, table, or constraint that a running application version still references.
   - Old and new application versions must both work against the expanded schema.
2. **Migrate** — deploy the application version that writes to the new columns/tables while still reading the old ones. Backfill of existing rows is handled by its own issue and must be idempotent and resumable.
3. **Contract** — only after every replica runs a version that no longer references the old shape, ship a cleanup migration that drops/renames the obsolete columns, tables, or constraints. Contract migrations are destructive and require the exemption process below.

### Ordering relative to application deploys

| Step | Migration | Application deploy |
|------|-----------|--------------------|
| 1 | Expand (additive) | — |
| 2 | — | Deploy version writing new + reading old |
| 3 | — | Backfill (separate issue) |
| 4 | — | Deploy version reading/writing new only |
| 5 | Contract (destructive) | — |

A migration must never require the new application code to be live before it is applied, and application code must never require a migration that has not yet been applied. When in doubt, split the change into more releases.

### Handling destructive changes

Destructive changes (dropping/renaming columns or tables, narrowing types, adding `NOT NULL` without a default, adding constraints that existing rows may violate) are only allowed in a contract migration. If a down path genuinely cannot exist, the PR MUST:

- Record a **dated exemption** in the table below, including the migration name, the reason, and the date it was reviewed.
- Link the exemption from the migration file header as a comment.
- Be reviewed by a second engineer in the PR.

#### Dated exemptions

| Migration | Destructive change | Reason no down path | Reviewed |
|-----------|--------------------|---------------------|----------|
| _(none yet)_ | | | |

Exemptions are reviewed at each release; stale entries are removed once the change is fully rolled out.

### Avoiding `ACCESS EXCLUSIVE` locks

On partitioned and high-volume tables, prefer operations that take weaker locks and split work into steps:

- **Indexes**: use `CREATE INDEX CONCURRENTLY` (outside a transaction) instead of `CREATE INDEX`. On partitioned tables create the index on the parent with `ONLY` and then attach per-partition indexes, or create per-partition indexes concurrently and attach them.
- **Constraints**: add `CHECK`/`FOREIGN KEY` constraints as `NOT VALID`, then run `VALIDATE CONSTRAINT` in a separate migration/transaction. `VALIDATE` takes only a `SHARE UPDATE EXCLUSIVE` lock.
- **`NOT NULL`**: add the column nullable, backfill, add a `NOT VALID` check, validate it, then set `NOT NULL` (PostgreSQL 12+ can use a validated check to avoid a full table scan).
- **Column type changes**: add a new column, dual-write, backfill, then contract the old column.
- **Defaults**: adding a column with a constant `DEFAULT` is metadata-only on PostgreSQL 11+; volatile defaults rewrite the table and must be avoided.

## Migration Linting and Rollback Rehearsal

CI enforces the policy above for every migration under `migrations/`:

- **Lint** (`cargo xtask migrate lint` / `scripts/migration_lint.sh`): flags obviously unsafe migrations — `DROP COLUMN`, `RENAME COLUMN`, `ADD COLUMN ... NOT NULL` without `DEFAULT`, `CREATE INDEX` without `CONCURRENTLY` on large/partitioned tables, and `ALTER TABLE ... SET NOT NULL` without a validated check. A deliberately unsafe fixture migration proves the linter fails.
- **Forward run**: all migrations are applied to an empty database.
- **Rollback rehearsal**: each migration's down path (or documented equivalent) is applied against a database seeded with representative data, verifying the schema is safely reversible.

The linter is a fast heuristic and does not prove data-dependent safety; reviewers remain responsible for judging backfills and constraint validity against production data.

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

## Partitioning Migration Process

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

## Swap market-data archive partitions (#1508)

`swap_quote_archive` and `swap_price_snapshots` use monthly range partitions managed by the SQL
functions `ensure_swap_archive_partitions(months_ahead)` and `detach_swap_archive_partitions(retention_months)`
(see `docs/swap-platform-hardening.md`). Detached partitions are tracked in `swap_archive_exports`
and dropped only after a successful export.
