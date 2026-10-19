-- Generalize the monthly partition routines so they can manage any
-- range-partitioned parent table (not just `transactions`), so the swap tables
-- reuse the same advisory-locked creation and retention logic.
--
-- ensure_partition_for(date) and detach_old_partitions(months) keep their
-- signatures and behavior for `transactions`; they now delegate to the
-- table-parameterized implementations below so there is a single code path.
-- Partition naming is unchanged: `<parent>_yYYYYmMM`.

CREATE OR REPLACE FUNCTION ensure_partition_for_table(parent_table TEXT, target_date DATE)
RETURNS void AS $$
DECLARE
    partition_date DATE;
    partition_name TEXT;
    start_date TEXT;
    end_date TEXT;
BEGIN
    partition_date := DATE_TRUNC('month', target_date);
    partition_name := parent_table || '_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
    start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
    end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

    -- Serialize concurrent callers targeting the same partition name.
    PERFORM pg_advisory_xact_lock(hashtext(partition_name));

    BEGIN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
            partition_name, parent_table, start_date, end_date
        );
    EXCEPTION WHEN duplicate_table THEN
        -- A concurrent caller already created it; treat as success.
        NULL;
    END;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION ensure_partition_for(target_date DATE)
RETURNS void AS $$
BEGIN
    PERFORM ensure_partition_for_table('transactions', target_date);
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION detach_old_partitions_for_table(parent_table TEXT, retention_months INTEGER)
RETURNS void AS $$
DECLARE
    partition_record RECORD;
    cutoff_date DATE;
    partition_date DATE;
BEGIN
    cutoff_date := DATE_TRUNC('month', NOW() - (retention_months || ' months')::INTERVAL);

    FOR partition_record IN
        SELECT c.relname AS partition_name
        FROM pg_class c
        JOIN pg_inherits i ON c.oid = i.inhrelid
        JOIN pg_class p ON i.inhparent = p.oid
        WHERE p.relname = parent_table
          AND c.relname ~ ('^' || parent_table || '_y\d{4}m\d{2}$')
    LOOP
        partition_date := TO_DATE(
            SUBSTRING(partition_record.partition_name FROM '_y(\d{4})m\d{2}$')
            || SUBSTRING(partition_record.partition_name FROM '_y\d{4}m(\d{2})$'),
            'YYYYMM'
        );

        IF partition_date < cutoff_date THEN
            EXECUTE format('ALTER TABLE %I DETACH PARTITION %I', parent_table, partition_record.partition_name);
            RAISE NOTICE 'Detached old partition: %', partition_record.partition_name;
        END IF;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION detach_old_partitions(retention_months INTEGER DEFAULT 12)
RETURNS void AS $$
BEGIN
    PERFORM detach_old_partitions_for_table('transactions', retention_months);
END;
$$ LANGUAGE plpgsql;
