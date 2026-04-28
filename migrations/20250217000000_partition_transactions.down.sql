-- Reverse the partitioned table migration: restore the plain transactions table
DROP FUNCTION IF EXISTS maintain_partitions();
DROP FUNCTION IF EXISTS detach_old_partitions(INTEGER);
DROP FUNCTION IF EXISTS create_monthly_partition();

-- Drop all partition children (dynamically, to handle any auto-created ones)
DO $$
DECLARE
    r RECORD;
BEGIN
    FOR r IN
        SELECT c.relname
        FROM pg_class c
        JOIN pg_inherits i ON c.oid = i.inhrelid
        JOIN pg_class p ON i.inhparent = p.oid
        WHERE p.relname = 'transactions'
    LOOP
        EXECUTE format('DROP TABLE IF EXISTS %I', r.relname);
    END LOOP;
END $$;

DROP TABLE IF EXISTS transactions;

-- Restore the original non-partitioned table if it still exists
ALTER TABLE IF EXISTS transactions_old RENAME TO transactions;
