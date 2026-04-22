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
