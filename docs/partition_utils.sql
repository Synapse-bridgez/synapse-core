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
