-- Partition Management Utilities
-- Run these commands manually for partition operations

-- ============================================
-- MONITORING QUERIES
-- ============================================

-- List all partitions with sizes
SELECT 
    c.relname AS partition_name,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS total_size,
    pg_size_pretty(pg_relation_size(c.oid)) AS table_size,
    pg_get_expr(c.relpartbound, c.oid) AS partition_bound,
    (SELECT COUNT(*) FROM ONLY c.*) AS row_count
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname;

-- Check total transactions count
SELECT COUNT(*) AS total_transactions FROM transactions;

-- Check transactions by partition
SELECT 
    tableoid::regclass AS partition_name,
    COUNT(*) AS row_count,
    MIN(created_at) AS oldest_record,
    MAX(created_at) AS newest_record
FROM transactions
GROUP BY tableoid
ORDER BY partition_name;

-- ============================================
-- MAINTENANCE OPERATIONS
-- ============================================

-- Run full maintenance (create new + detach old)
SELECT maintain_partitions();

-- Create next month's partition only
SELECT create_monthly_partition();

-- Detach partitions older than 12 months
SELECT detach_old_partitions(12);

-- Detach partitions older than 6 months (custom retention)
SELECT detach_old_partitions(6);

-- ============================================
-- MANUAL PARTITION CREATION
-- ============================================

-- Create specific partition (example for May 2025)
CREATE TABLE IF NOT EXISTS transactions_y2025m05 PARTITION OF transactions
    FOR VALUES FROM ('2025-05-01') TO ('2025-06-01');

-- Create multiple future partitions
DO $$
DECLARE
    start_date DATE;
    end_date DATE;
    partition_name TEXT;
    i INTEGER;
BEGIN
    FOR i IN 0..11 LOOP
        start_date := DATE_TRUNC('month', NOW() + (i || ' months')::INTERVAL);
        end_date := start_date + INTERVAL '1 month';
        partition_name := 'transactions_y' || TO_CHAR(start_date, 'YYYY') || 'm' || TO_CHAR(start_date, 'MM');
        
        IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                partition_name, start_date, end_date
            );
            RAISE NOTICE 'Created partition: %', partition_name;
        END IF;
    END LOOP;
END $$;

-- ============================================
-- ARCHIVAL OPERATIONS
-- ============================================

-- Detach specific partition manually
ALTER TABLE transactions DETACH PARTITION transactions_y2024m01;

-- Re-attach a partition
ALTER TABLE transactions ATTACH PARTITION transactions_y2024m01
    FOR VALUES FROM ('2024-01-01') TO ('2024-02-01');

-- Move detached partition to archive schema
CREATE SCHEMA IF NOT EXISTS archive;
ALTER TABLE transactions_y2024m01 SET SCHEMA archive;

-- Export partition to CSV
\copy transactions_y2024m01 TO '/tmp/transactions_2024_01.csv' CSV HEADER;

-- Drop old partition after archival
DROP TABLE IF EXISTS transactions_y2024m01;

-- ============================================
-- SWAP TABLES (swap_orders, swap_quotes, swap_fills, swap_events)
-- ============================================

-- Create swap partitions for the current month plus the next two
SELECT ensure_swap_partitions(3);

-- Create swap partitions for one specific month (backfill)
SELECT ensure_swap_partition_for('2027-01-15');

-- Detach swap partitions older than 12 months (skips any partition still
-- holding non-terminal orders or unpublished swap_events)
SELECT detach_swap_partitions(12);

-- List swap partitions and whether they are attached to their parent
SELECT
    c.relname AS partition_name,
    p.relname AS parent_table,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS total_size
FROM pg_class c
LEFT JOIN pg_inherits i ON c.oid = i.inhrelid
LEFT JOIN pg_class p ON i.inhparent = p.oid
WHERE c.relname ~ '^swap_[a-z]+_y[0-9]{4}m[0-9]{2}$'
ORDER BY c.relname;

-- Find non-terminal orders that block detachment of older partitions
SELECT id, status, created_at
FROM swap_orders
WHERE status NOT IN ('completed', 'failed', 'cancelled')
ORDER BY created_at
LIMIT 50;

-- Find unpublished outbox rows that block detachment of swap_events partitions
SELECT id, aggregate_id, event_type, created_at
FROM swap_events
WHERE published_at IS NULL
ORDER BY created_at
LIMIT 50;

-- Archive a detached swap partition (same pattern as transactions)
-- CREATE SCHEMA IF NOT EXISTS archive;
-- ALTER TABLE swap_orders_y2025m01 SET SCHEMA archive;

-- ============================================
-- PERFORMANCE ANALYSIS
-- ============================================

-- Check partition pruning (EXPLAIN should show only relevant partitions)
EXPLAIN (ANALYZE, BUFFERS)
SELECT * FROM transactions 
WHERE created_at >= '2025-02-01' 
  AND created_at < '2025-03-01'
  AND status = 'pending';

-- Index usage statistics per partition
SELECT 
    schemaname,
    tablename,
    indexname,
    idx_scan,
    idx_tup_read,
    idx_tup_fetch
FROM pg_stat_user_indexes
WHERE tablename LIKE 'transactions_y%'
ORDER BY tablename, indexname;

-- ============================================
-- CLEANUP
-- ============================================

-- Drop old table after verifying migration
DROP TABLE IF EXISTS transactions_old;

-- Vacuum all partitions
VACUUM ANALYZE transactions;

-- Reindex all partitions
REINDEX TABLE transactions;
