-- Fix a real bug: create_monthly_partition() only ever ensured the
-- "NOW() + 2 months" partition existed. That's fine as long as the app has
-- been running continuously (each day's cron tick keeps a rolling 2-month
-- buffer ahead), but it never backfills the *current* month if it happens to
-- be missing (e.g. a fresh deploy, a long app outage spanning a month
-- boundary, or a freshly migrated test database created after the static
-- seed list in 20260422000000_ensure_current_partitions.sql has run out).
--
-- Rewrite it to ensure the current month plus the following two months all
-- exist, idempotently, so it self-heals regardless of when it's first
-- called relative to "now".
CREATE OR REPLACE FUNCTION create_monthly_partition()
RETURNS void AS $$
DECLARE
    month_offset INT;
    partition_date DATE;
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
BEGIN
    FOR month_offset IN 0..2 LOOP
        partition_date := DATE_TRUNC('month', NOW() + (month_offset || ' months')::interval);
        partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
        start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
        end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

        IF NOT EXISTS (
            SELECT 1 FROM pg_class WHERE relname = partition_name
        ) THEN
            EXECUTE format(
                'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                partition_name, start_date, end_date
            );
            RAISE NOTICE 'Created partition: %', partition_name;
        END IF;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Backfill immediately so any database migrated with this version always has
-- a partition covering the current month, instead of waiting for the
-- application's periodic PartitionManager tick.
SELECT create_monthly_partition();
