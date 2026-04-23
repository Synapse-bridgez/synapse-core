-- Ensure partitions exist from 2025-01 through 3 months ahead of now.
-- This is idempotent: it skips months that already have a partition.
DO $$
DECLARE
    cur_date  DATE := DATE '2025-01-01';
    end_date  DATE := DATE_TRUNC('month', NOW()) + INTERVAL '3 months';
    p_name    TEXT;
BEGIN
    WHILE cur_date < end_date LOOP
        p_name := 'transactions_y' || TO_CHAR(cur_date, 'YYYY') || 'm' || TO_CHAR(cur_date, 'MM');
        IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = p_name) THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                p_name,
                TO_CHAR(cur_date, 'YYYY-MM-DD'),
                TO_CHAR(cur_date + INTERVAL '1 month', 'YYYY-MM-DD')
            );
        END IF;
        cur_date := cur_date + INTERVAL '1 month';
    END LOOP;
END $$;
