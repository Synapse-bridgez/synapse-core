-- Part A fix: create_monthly_partition() was a check-then-act race under
-- concurrent callers — its "IF NOT EXISTS (SELECT ...)" existence check and its
-- "CREATE TABLE" were two separate statements with no lock between them. Two
-- sessions could both pass the check before either committed, and the second
-- CREATE TABLE for the same partition name would raise a duplicate_table error.
--
-- ensure_partition_for(target_date) replaces the inline check-then-act with:
--   1. a pg_advisory_xact_lock keyed on the partition name, so concurrent
--      callers targeting the same partition serialize instead of racing
--      (the lock auto-releases at transaction end — no explicit unlock needed);
--   2. CREATE TABLE IF NOT EXISTS instead of a separate existence check; and
--   3. an exception handler that treats a losing duplicate_table race as
--      success rather than propagating it, as defense in depth.
--
-- It also generalizes create_monthly_partition()'s hardcoded "next + 2 months"
-- target into an arbitrary-date parameter, so it can be called synchronously
-- from the Rust-side 23514 self-heal path (insert_transaction) for whatever
-- month the failing insert actually needed, not just the next scheduled one.

CREATE OR REPLACE FUNCTION ensure_partition_for(target_date DATE)
RETURNS void AS $$
DECLARE
    partition_date DATE;
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
BEGIN
    partition_date := DATE_TRUNC('month', target_date);
    partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
    start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

    -- Serialize concurrent callers targeting the same partition name.
    PERFORM pg_advisory_xact_lock(hashtext(partition_name));

    BEGIN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_date, end_date
        );
    EXCEPTION WHEN duplicate_table THEN
        -- A concurrent caller already created it; treat as success.
        NULL;
    END;
END;
$$ LANGUAGE plpgsql;

-- Preserve create_monthly_partition()'s existing scheduled-maintenance
-- contract ("create next + 2 months' partition") but delegate to the
-- concurrency-safe function above instead of duplicating the race.
CREATE OR REPLACE FUNCTION create_monthly_partition()
RETURNS void AS $$
BEGIN
    PERFORM ensure_partition_for((NOW() + INTERVAL '2 months')::date);
END;
$$ LANGUAGE plpgsql;
