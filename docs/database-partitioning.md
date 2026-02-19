# Database Partitioning Implementation

## Overview

This document describes the implementation of **time-based database partitioning** for the `transactions` table in Synapse Core. Partitioning improves query performance, reduces lock contention, and enables efficient data retention policies for high-volume transaction scenarios (millions of records).

## Why Partitioning?

As the transaction table grows to millions of records, performance issues emerge:
- **Slow queries**: Full table scans become increasingly expensive
- **VACUUM overhead**: PostgreSQL must scan huge tables for dead tuples
- **Index bloat**: Indexes on large tables consume significant memory
- **Lock contention**: Concurrent operations wait longer for locks on large tables

Partitioning solves these by:
- Enabling partition elimination in query plans (only scanning relevant partitions)
- Reducing vacuum scope per partition
- Allowing independent index management per partition
- Enabling efficient archival of old data

## Implementation Details

### Partitioning Strategy

The `transactions` table uses **declarative range partitioning** based on `created_at` timestamps:

```
transactions (partitioned table)
├── transactions_y2025m01 (Jan 2025: 2025-01-01 to 2025-02-01)
├── transactions_y2025m02 (Feb 2025: 2025-02-01 to 2025-03-01)
├── transactions_y2025m03 (Mar 2025: 2025-03-01 to 2025-04-01)
├── transactions_y2025m04 (Apr 2025: 2025-04-01 to 2025-05-01)
└── ... (additional months)
```

Each partition covers exactly one calendar month.

### Partition Naming Convention

Partitions follow the naming pattern: `transactions_yYYYYmMM`

- `YYYY`: 4-digit year
- `mm`: 2-digit zero-padded month
- Examples: `transactions_y2025m01`, `transactions_y2025m12`, `transactions_y2026m01`

### Primary Key

The primary key is composite:
```sql
PRIMARY KEY (id, created_at)
```

The `created_at` column must be included in the primary key to support uniqueness constraints across partitions.

### Indexes

Each partition has three indexes for optimal query performance:

1. **Status Index**: `idx_transactions_status`
   - Supports queries filtering by transaction status
   - Example: `WHERE status = 'completed'`

2. **Stellar Account Index**: `idx_transactions_stellar_account`
   - Supports queries filtering by account
   - Example: `WHERE stellar_account = 'GABCD...'`

3. **Created At Index**: `idx_transactions_created_at`
   - Supports partition pruning
   - Enables range queries on timestamps

## Partition Management

### Automatic Partition Creation

The `PartitionManager` ensures upcoming partitions exist automatically:

```rust
// Ensure partitions exist for the next 3 months
PartitionManager::ensure_future_partitions(pool, 3).await?;
```

This is called daily by the background maintenance job (`src/db/cron.rs`).

### Retention Policy

Old partitions can be automatically archived or deleted based on a retention policy:

```rust
// Archive partitions older than 12 months
PartitionManager::archive_old_partitions(pool, 12).await?;
```

Archiving detaches partitions from the parent table, allowing them to be:
- Backed up separately
- Compressed with different storage parameters
- Dropped to reclaim disk space

### Partition Statistics

The `ANALYZE` command updates table statistics for the query planner:

```rust
PartitionManager::analyze_partitions(pool).await?;
```

This should run regularly to maintain query performance.

## Usage Examples

### Creating a Specific Partition

```rust
// Create partition for February 2025
db::cron::PartitionManager::create_partition(&pool, 2025, 2).await?;
```

### Running Full Maintenance

```rust
// Ensure future partitions, archive old ones, analyze statistics
db::cron::PartitionManager::run_maintenance(&pool).await?;
```

This is the main entry point, called daily by the background job in `main.rs`.

### Querying Partitioned Data

Queries work transparently with partitioned tables:

```sql
-- Query a specific month's data
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' 
  AND created_at < '2025-03-01';

-- Query across multiple months
SELECT * FROM transactions 
WHERE created_at >= '2025-01-01' 
  AND created_at < '2025-03-01'
  AND status = 'completed';
```

The query planner automatically applies **partition elimination** to only scan relevant partitions.

## Migration Process

The migration file `20250219000000_implement_db_partitioning.sql` performs:

1. **Rename old table**: `transactions → transactions_old`
2. **Create partitioned table**: New `transactions` with PARTITION BY RANGE
3. **Create monthly partitions**: For 2025-2026
4. **Create indexes**: On each partition for status, account, created_at
5. **Migrate data**: From `transactions_old` → partitioned `transactions`
6. **Drop old table**: Clean up `transactions_old`
7. **Create helper function**: For manual partition creation

## Requirements

- **PostgreSQL 14+**: Declarative partitioning is the recommended approach in PG 14+
- **No pg_partman extension required**: Built on native PostgreSQL features
- **Partial pg_partman compatibility**: The partition naming format matches pg_partman conventions for easier migration in the future

## Performance Impact

### Expected Improvements

- **Query Performance**: 50-90% faster for queries with `created_at` predicates
- **Index Size**: ~2/12 of monolithic table index (for 1-month partitions)
- **VACUUM Time**: Proportional to partition size, not full table
- **Lock Contention**: Reduced due to smaller per-partition locks

### Partition-Aware Queries

Queries that include `created_at` range predicates benefit most:

```sql
-- GOOD: Query planner can eliminate partitions
SELECT COUNT(*) FROM transactions 
WHERE created_at >= '2025-02-01' AND created_at < '2025-03-01';

-- LESS EFFECTIVE: Must scan all partitions
SELECT COUNT(*) FROM transactions 
WHERE status = 'completed';  -- No date filter
```

## Maintenance Operations

### Monitoring Partition Health

Check partition sizes:

```sql
SELECT schemaname, tablename, pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) as size
FROM pg_tables
WHERE tablename LIKE 'transactions_y%m%'
ORDER BY tablename;
```

### Archiving Partitions Manually

```sql
-- Detach a partition for archival
ALTER TABLE transactions DETACH PARTITION transactions_y2026m01 FINALIZE;

-- Drop if no longer needed
DROP TABLE transactions_y2026m01;

-- Or export to archive storage
pg_dump -t transactions_y2026m01 > archive_y2026m01.sql
```

### Reattaching Archived Partitions

```sql
-- Reattach a previously detached partition
ALTER TABLE transactions ATTACH PARTITION transactions_y2026m01
  FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');
```

## Configuration

### Partition Creation Interval

In `src/main.rs`, the background job runs daily:

```rust
let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
```

Adjust the duration (in seconds) to change frequency:
- 3600 = hourly
- 86400 = daily (default)
- 604800 = weekly

### Months Ahead

In `src/db/cron.rs`, ensure partitions 3 months ahead:

```rust
Self::ensure_future_partitions(pool, 3).await?;
```

Change the argument to create more/fewer ahead-of-time partitions.

### Retention Period

In `src/db/cron.rs`, archive partitions older than 12 months:

```rust
Self::archive_old_partitions(pool, 12).await?;
```

Change the argument to adjust retention (in months).

## Testing

Run the partition management tests:

```bash
cargo test db::cron
```

Tests validate:
- Partition name formatting
- Month calculation for future partitions
- Date boundary calculations

## Future Enhancements

1. **pg_partman Integration**: Use `pg_partman` extension for more advanced partition management
2. **Time-Travel Queries**: Archive partitions to cold storage (e.g., S3)
3. **Parallel Inserts**: Leverage partitions for parallel loading
4. **Dynamic Partition Sizing**: Adjust partition granularity (weekly, daily) based on volume
5. **Smart Archival**: Move old partitions to different tablespaces with different storage parameters

## Troubleshooting

### Cannot compile due to DATABASE_URL

If you get `set DATABASE_URL to use query macros online` error:

```bash
# Set DATABASE_URL temporarily for compilation
export DATABASE_URL=postgres://user:pass@localhost:5432/synapse
cargo build

# Or use sqlx offline mode (requires prior setup)
cargo sqlx prepare
cargo build
```

### Partition not created

Check the application logs:
```bash
RUST_LOG=debug cargo run
```

Monitor the background job output for errors.

### Query still slow despite partitioning

Ensure the query includes a `created_at` range predicate:
```sql
-- Add date filter for partition elimination
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' AND created_at < '2025-03-01';
```

Check partition statistics:
```sql
ANALYZE transactions;
```

## References

- [PostgreSQL Declarative Partitioning](https://www.postgresql.org/docs/current/ddl-partitioning.html)
- [PostgreSQL Partitioning Benefits](https://www.postgresql.org/docs/current/ddl-partitioning-benefits.html)
- [pg_partman Extension](https://github.com/pgpartman/pg_partman) (for advanced features)
- [sqlx Documentation](https://github.com/launchbadge/sqlx)
