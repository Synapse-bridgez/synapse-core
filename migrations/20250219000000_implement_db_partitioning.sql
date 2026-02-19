-- Migration: Implement time-based partitioning for transactions table
-- This migration converts the transactions table to use declarative partitioning by created_at
-- Requirements: PostgreSQL 14+

-- Step 1: Rename the current transactions table
ALTER TABLE IF EXISTS transactions RENAME TO transactions_old;

-- Step 2: Drop old indexes (they are not compatible with partitioned tables for constraints)
DROP INDEX IF EXISTS idx_transactions_status;
DROP INDEX IF EXISTS idx_transactions_stellar_account;

-- Step 3: Create the new partitioned transactions table
CREATE TABLE transactions (
    id UUID NOT NULL DEFAULT gen_random_uuid(),
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    anchor_transaction_id VARCHAR(255),
    callback_type VARCHAR(20),
    callback_status VARCHAR(20),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

-- Step 4: Create monthly partitions
-- We'll create partitions for the current year (2025) and future months
-- Format: transactions_y{YEAR}m{MONTH}

-- January 2025
CREATE TABLE transactions_y2025m01 PARTITION OF transactions
    FOR VALUES FROM ('2025-01-01') TO ('2025-02-01');

-- February 2025
CREATE TABLE transactions_y2025m02 PARTITION OF transactions
    FOR VALUES FROM ('2025-02-01') TO ('2025-03-01');

-- March 2025
CREATE TABLE transactions_y2025m03 PARTITION OF transactions
    FOR VALUES FROM ('2025-03-01') TO ('2025-04-01');

-- April 2025
CREATE TABLE transactions_y2025m04 PARTITION OF transactions
    FOR VALUES FROM ('2025-04-01') TO ('2025-05-01');

-- May 2025
CREATE TABLE transactions_y2025m05 PARTITION OF transactions
    FOR VALUES FROM ('2025-05-01') TO ('2025-06-01');

-- June 2025
CREATE TABLE transactions_y2025m06 PARTITION OF transactions
    FOR VALUES FROM ('2025-06-01') TO ('2025-07-01');

-- July 2025
CREATE TABLE transactions_y2025m07 PARTITION OF transactions
    FOR VALUES FROM ('2025-07-01') TO ('2025-08-01');

-- August 2025
CREATE TABLE transactions_y2025m08 PARTITION OF transactions
    FOR VALUES FROM ('2025-08-01') TO ('2025-09-01');

-- September 2025
CREATE TABLE transactions_y2025m09 PARTITION OF transactions
    FOR VALUES FROM ('2025-09-01') TO ('2025-10-01');

-- October 2025
CREATE TABLE transactions_y2025m10 PARTITION OF transactions
    FOR VALUES FROM ('2025-10-01') TO ('2025-11-01');

-- November 2025
CREATE TABLE transactions_y2025m11 PARTITION OF transactions
    FOR VALUES FROM ('2025-11-01') TO ('2025-12-01');

-- December 2025
CREATE TABLE transactions_y2025m12 PARTITION OF transactions
    FOR VALUES FROM ('2025-12-01') TO ('2026-01-01');

-- January 2026
CREATE TABLE transactions_y2026m01 PARTITION OF transactions
    FOR VALUES FROM ('2026-01-01') TO ('2026-02-01');

-- February 2026
CREATE TABLE transactions_y2026m02 PARTITION OF transactions
    FOR VALUES FROM ('2026-02-01') TO ('2026-03-01');

-- Step 5: Create indexes on each partition for faster queries
-- Status index on each partition
CREATE INDEX idx_transactions_status ON transactions(status);

-- Stellar account index on each partition  
CREATE INDEX idx_transactions_stellar_account ON transactions(stellar_account);

-- Created_at index to support partition pruning
CREATE INDEX idx_transactions_created_at ON transactions(created_at);

-- Step 6: Migrate data from old table to new partitioned table
INSERT INTO transactions (
    id, stellar_account, amount, asset_code, status,
    created_at, updated_at, anchor_transaction_id, callback_type, callback_status
)
SELECT 
    id, stellar_account, amount, asset_code, status,
    created_at, updated_at, anchor_transaction_id, callback_type, callback_status
FROM transactions_old
WHERE (created_at IS NOT NULL);

-- Step 7: Drop the old table
DROP TABLE IF EXISTS transactions_old;

-- Step 8: Create a function to automatically create new partitions
-- This function creates a new partition for the given month
CREATE OR REPLACE FUNCTION create_monthly_partition(table_name TEXT, year INT, month INT)
RETURNS void AS $$
DECLARE
    partition_name TEXT;
    start_date DATE;
    end_date DATE;
BEGIN
    partition_name := format('transactions_y%sm%s', year, LPAD(month::TEXT, 2, '0'));
    start_date := make_date(year, month, 1);
    end_date := start_date + interval '1 month';
    
    -- Check if partition already exists
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.tables 
        WHERE table_name = partition_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_date, end_date
        );
        RAISE NOTICE 'Created partition: %', partition_name;
    ELSE
        RAISE NOTICE 'Partition %s already exists', partition_name;
    END IF;
END;
$$ LANGUAGE plpgsql;
