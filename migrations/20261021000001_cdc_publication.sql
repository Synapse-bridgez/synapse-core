-- CDC publication for analytics export (issue #1514). Created only for tables
-- that exist; the connector owns slot creation (see docs/cdc.md).
DO $$
DECLARE tbls text;
BEGIN
  SELECT string_agg(format('%I', tablename), ', ') INTO tbls
  FROM pg_tables
  WHERE schemaname = 'public' AND tablename IN ('transactions', 'audit_logs');
  IF tbls IS NOT NULL AND NOT EXISTS (SELECT 1 FROM pg_publication WHERE pubname = 'synapse_swap_cdc') THEN
    EXECUTE 'CREATE PUBLICATION synapse_swap_cdc FOR TABLE ' || tbls
            || ' WITH (publish_via_partition_root = true)';
  END IF;
END $$;
