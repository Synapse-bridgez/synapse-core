-- Migration: Convert transactions table to partitioned table
-- This enables time-based partitioning for high-volume scaling

-- Step 1: Rename existing table
ALTER TABLE IF EXISTS transactions RENAME TO transactions_old;

-- Step 2: Create partitioned table
CREATE TABLE transactions (
    id UUID DEFAULT gen_random_uuid(),
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

-- Step 3: Create indexes on partitioned table
CREATE INDEX idx_transactions_status ON transactions(status);
CREATE INDEX idx_transactions_stellar_account ON transactions(stellar_account);
CREATE INDEX idx_transactions_created_at ON transactions(created_at);

-- Step 4: Create initial partitions (current month + next 2 months)
CREATE TABLE transactions_y2025m02 PARTITION OF transactions
    FOR VALUES FROM ('2025-02-01') TO ('2025-03-01');

CREATE TABLE transactions_y2025m03 PARTITION OF transactions
    FOR VALUES FROM ('2025-03-01') TO ('2025-04-01');

CREATE TABLE transactions_y2025m04 PARTITION OF transactions
    FOR VALUES FROM ('2025-04-01') TO ('2025-05-01');

-- Step 5: Migrate existing data (if any)
INSERT INTO transactions 
SELECT * FROM transactions_old
ON CONFLICT DO NOTHING;

-- Step 6: Create function to auto-create monthly partitions
CREATE OR REPLACE FUNCTION create_monthly_partition()
RETURNS void AS $$
DECLARE
    partition_date DATE;
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
BEGIN
    -- Create partition for next month
    partition_date := DATE_TRUNC('month', NOW() + INTERVAL '2 months');
    partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
    start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');
    
    -- Check if partition already exists
    IF NOT EXISTS (
        SELECT 1 FROM pg_class WHERE relname = partition_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_date, end_date
        );
        RAISE NOTICE 'Created partition: %', partition_name;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Step 7: Create function to detach old partitions (retention policy)
CREATE OR REPLACE FUNCTION detach_old_partitions(retention_months INTEGER DEFAULT 12)
RETURNS void AS $$
DECLARE
    partition_record RECORD;
    cutoff_date DATE;
BEGIN
    cutoff_date := DATE_TRUNC('month', NOW() - (retention_months || ' months')::INTERVAL);
    
    FOR partition_record IN
        SELECT
            c.relname AS partition_name,
            pg_get_expr(c.relpartbound, c.oid) AS partition_bound
        FROM pg_class c
        JOIN pg_inherits i ON c.oid = i.inhrelid
        JOIN pg_class p ON i.inhparent = p.oid
        WHERE p.relname = 'transactions'
        AND c.relname LIKE 'transactions_y%'
    LOOP
        -- Extract year and month from partition name
        IF partition_record.partition_name ~ 'transactions_y\d{4}m\d{2}' THEN
            DECLARE
                partition_date DATE;
                year_part TEXT;
                month_part TEXT;
            BEGIN
                year_part := SUBSTRING(partition_record.partition_name FROM 'y(\d{4})');
                month_part := SUBSTRING(partition_record.partition_name FROM 'm(\d{2})');
                partition_date := (year_part || '-' || month_part || '-01')::DATE;
                
                IF partition_date < cutoff_date THEN
                    EXECUTE format('ALTER TABLE transactions DETACH PARTITION %I', partition_record.partition_name);
                    RAISE NOTICE 'Detached old partition: %', partition_record.partition_name;
                END IF;
            END;
        END IF;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Step 8: Create maintenance function (combines both operations)
CREATE OR REPLACE FUNCTION maintain_partitions()
RETURNS void AS $$
BEGIN
    PERFORM create_monthly_partition();
    PERFORM detach_old_partitions(12); -- Keep 12 months by default
END;
$$ LANGUAGE plpgsql;

-- Optional: Drop old table after verifying migration
-- DROP TABLE IF EXISTS transactions_old;
