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

> These SQL functions are the fallback path. Application code should use the
> `PartitionManager` helpers documented in [Public Helpers](#public-helpers-srcdbpartitionrs);
> see also [`partition_utils.sql`](./partition_utils.sql).

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
let manager = PartitionManager::new(pool.clone(), 24, Some(cache));
manager.start();
```

### Public Helpers (`src/db/partition.rs`)

These are the supported entry points for partition maintenance from Rust code.
Prefer them over hand-written SQL; the SQL in
[`partition_utils.sql`](./partition_utils.sql) is the fallback path (see below).

#### `PartitionManager::new(pool: PgPool, interval_hours: u64, cache: Option<QueryCache>) -> Self`

Constructs a manager. `interval_hours` is how often the background task runs
maintenance (24 is the production default). `cache` is optional; when present it
is warmed after a new partition is created.

```rust
let manager = PartitionManager::new(pool.clone(), 24, Some(cache));
```

#### `PartitionManager::has_cache(&self) -> bool`

Returns whether the manager was constructed with a cache. Use it as a
startup-time assertion so a construction-order regression (building the manager
before the cache exists) fails loudly instead of silently disabling cache
warming.

```rust
assert!(manager.has_cache(), "PartitionManager must be built after the cache");
```

#### `PartitionManager::start(self)`

Spawns the background maintenance loop (create upcoming partitions, detach old
ones) on the Tokio runtime. The first tick is skipped, so the first run happens
one `interval_hours` after startup. Errors are logged, not propagated.

```rust
manager.start();
```

#### `PartitionManager::create_partition(&self) -> Result<bool, sqlx::Error>`

Manually triggers creation of the partition two months ahead. Returns `true`
when a new partition was created and `false` when it already existed. On
creation, the query cache is warmed if one was supplied. Use it for
backfills, tests, or an admin "ensure future partitions" action.

```rust
if manager.create_partition().await? {
    tracing::info!("created upcoming partition");
}
```

#### `PartitionManager::detach_old_partitions(&self, retention_months: i32) -> Result<(), sqlx::Error>`

Detaches partitions older than `retention_months` (the default policy is 12).
Detached tables are kept as regular tables for archival. Use it to apply a
custom retention window without waiting for the scheduled job.

```rust
manager.detach_old_partitions(6).await?; // Keep 6 months
```

> `maintain_partitions()` is private: it is only invoked by the background loop
> started via `start()`. To run it on demand, call `create_partition()` and
> `detach_old_partitions()` explicitly.

### Fallback: Manual SQL

The SQL below (and in [`partition_utils.sql`](./partition_utils.sql)) is the
fallback path for when the application is not running — for example during an
incident, a one-off ops task, or a psql session. Prefer the Rust helpers above
in application code.

```sql
-- Full maintenance (create new + detach old)
SELECT maintain_partitions();

-- Create the upcoming partition only
SELECT create_monthly_partition();

-- Detach partitions older than 6 months
SELECT detach_old_partitions(6);
```

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
