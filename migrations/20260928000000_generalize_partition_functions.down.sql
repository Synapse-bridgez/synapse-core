-- Restore the transactions-only partition routines and drop the generalized ones.

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

    PERFORM pg_advisory_xact_lock(hashtext(partition_name));

    BEGIN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
            partition_name, start_date, end_date
        );
    EXCEPTION WHEN duplicate_table THEN
        NULL;
    END;
END;
$$ LANGUAGE plpgsql;

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

DROP FUNCTION IF EXISTS detach_old_partitions_for_table(TEXT, INTEGER);
DROP FUNCTION IF EXISTS ensure_partition_for_table(TEXT, DATE);
