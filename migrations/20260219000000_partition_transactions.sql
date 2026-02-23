-- Migration: Convert `transactions` to a partitioned table by created_at
-- 1) rename existing table to transactions_old
-- 2) create partitioned parent `transactions`
-- 3) create monthly partitions for existing data and move rows

BEGIN;

-- rename existing table so we can recreate `transactions` as partitioned
ALTER TABLE IF EXISTS transactions RENAME TO transactions_old;

-- create partitioned parent table
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    anchor_transaction_id VARCHAR(255),
    callback_type VARCHAR(20),
    callback_status VARCHAR(20)
) PARTITION BY RANGE (created_at);

-- default partition to accept anything during migration (safety)
CREATE TABLE IF NOT EXISTS transactions_default PARTITION OF transactions DEFAULT;

-- helper function: create a monthly partition for given year/month
CREATE OR REPLACE FUNCTION create_transactions_month_partition(p_year int, p_month int)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
  p_start timestamptz := (make_timestamptz(p_year, p_month, 1, 0, 0, 0));
  p_end timestamptz := (p_start + INTERVAL '1 month');
  part_name text := format('transactions_y%sm%s', p_year, lpad(p_month::text,2,'0'));
BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = part_name) THEN
    EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', part_name, p_start, p_end);
    EXECUTE format('CREATE INDEX IF NOT EXISTS idx_%s_status ON %I (status)', part_name, part_name);
    EXECUTE format('CREATE INDEX IF NOT EXISTS idx_%s_stellar_account ON %I (stellar_account)', part_name, part_name);
  END IF;
END;
$$;

-- For each distinct year/month present in transactions_old, create a partition and move rows
DO $$
DECLARE
  r RECORD;
  part text;
BEGIN
  IF EXISTS (SELECT 1 FROM pg_class WHERE relname = 'transactions_old') THEN
    FOR r IN SELECT DISTINCT date_part('year', created_at)::int AS yy, date_part('month', created_at)::int AS mm FROM transactions_old WHERE created_at IS NOT NULL ORDER BY yy, mm LOOP
      PERFORM create_transactions_month_partition(r.yy, r.mm);
      part := format('transactions_y%sm%s', r.yy, lpad(r.mm::text,2,'0'));
      EXECUTE format('INSERT INTO %I SELECT * FROM transactions_old WHERE date_part(''year'', created_at)::int = %s AND date_part(''month'', created_at)::int = %s', part, r.yy, r.mm);
    END LOOP;

    -- Move any rows with NULL created_at (if any) into default partition
    EXECUTE 'INSERT INTO transactions SELECT * FROM transactions_old WHERE created_at IS NULL';

    -- After migrating data, drop the old table
    DROP TABLE transactions_old;
  END IF;
END;
$$;

COMMIT;

-- Notes:
-- - This migration creates a helper SQL function `create_transactions_month_partition` which can be reused
-- - Future partitions should be created ahead of time (via cron or pg_partman). The application includes a small maintenance task to do this.
