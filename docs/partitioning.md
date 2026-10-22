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
