# Database Partitioning - Quick Reference

## 🔍 Check Partition Status

```sql
-- List all partitions
SELECT c.relname, pg_size_pretty(pg_total_relation_size(c.oid)) AS size
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname;

-- Count records per partition
SELECT tableoid::regclass, COUNT(*) 
FROM transactions 
GROUP BY tableoid;
```

## 🛠️ Manual Operations

```sql
-- Create next month's partition
SELECT create_monthly_partition();

-- Detach old partitions (12 months)
SELECT detach_old_partitions(12);

-- Run full maintenance
SELECT maintain_partitions();
```

## 🚀 Application Control

```rust
// In your code
use synapse_core::db::partition::PartitionManager;

// Create manager (24-hour interval)
let manager = PartitionManager::new(pool.clone(), 24);
manager.start();

// Manual operations
manager.create_partition().await?;
manager.detach_old_partitions(6).await?;
```

## 📊 Performance Testing

```sql
-- Test partition pruning
EXPLAIN ANALYZE
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' 
  AND created_at < '2025-03-01';
```

## 🗄️ Archival

```sql
-- Detach specific partition
ALTER TABLE transactions DETACH PARTITION transactions_y2024m01;

-- Move to archive schema
CREATE SCHEMA IF NOT EXISTS archive;
ALTER TABLE transactions_y2024m01 SET SCHEMA archive;

-- Export to CSV
\copy transactions_y2024m01 TO '/tmp/archive_2024_01.csv' CSV HEADER;

-- Drop after backup
DROP TABLE transactions_y2024m01;
```

## 🔄 Rollback

```sql
DROP TABLE IF EXISTS transactions;
ALTER TABLE transactions_old RENAME TO transactions;
```

## 📁 Files Reference

- **Migration**: `migrations/20250217000000_partition_transactions.sql`
- **Manager**: `src/db/partition.rs`
- **Utilities**: `migrations/partition_utils.sql`
- **Docs**: `docs/partitioning.md`

## ⚙️ Configuration

- **Interval**: 24 hours (configurable in `main.rs`)
- **Retention**: 12 months (configurable in SQL function)
- **Partition Type**: Monthly by `created_at`
- **Naming**: `transactions_y{YYYY}m{MM}`
