-- Swap order, quote, and fill lookup indexes (#1526).
--
-- Access patterns and rationale: docs/swap-query-plans.md. Query text:
-- src/swap/persistence/queries.rs.
--
-- The swap tables are created (and partitioned by created_at) by the swap
-- persistence/partitioning work, which is out of scope here. This migration
-- defines ensure_swap_lookup_indexes() and calls it once; any table that does
-- not exist yet is skipped, and the migration that creates the swap tables
-- must `SELECT ensure_swap_lookup_indexes();` after creating them.
--
-- Index design:
-- * every index leads with tenant_id (RLS scope + highest-selectivity filter);
-- * range/list indexes end in (created_at DESC, id DESC) to match the
--   ORDER BY and to keep each partition's index small and prunable;
-- * status is never indexed alone (low cardinality); it only appears after
--   tenant_id and before created_at;
-- * idempotency_key / external_ref indexes are partial (NOT NULL) to limit
--   write amplification and bloat.
-- On partitioned parents CREATE INDEX cascades to every partition and to
-- partitions attached later, so no per-partition maintenance is needed.
--
-- Locking: the swap tables do not exist when this migration runs, so it builds
-- nothing. The definitions are kept as data and run via EXECUTE by
-- ensure_swap_lookup_indexes(), which is intended to be called in the same
-- migration that creates the (still empty) swap tables - an empty, brand-new
-- table has no writers to block, the same exemption
-- scripts/check-migration-safety.sh grants to tables created in-migration.
-- Indexes added later to populated swap tables must follow
-- docs/hot-table-index-migrations.md instead.

CREATE OR REPLACE FUNCTION ensure_swap_lookup_indexes() RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    -- (table, index name, column list [, partial predicate])
    defs text[][] := ARRAY[
        ['swap_orders', 'idx_swap_orders_tenant_status_created', 'tenant_id, status, created_at DESC, id DESC', ''],
        ['swap_orders', 'idx_swap_orders_tenant_created',        'tenant_id, created_at DESC, id DESC', ''],
        ['swap_orders', 'idx_swap_orders_tenant_pair_created',   'tenant_id, source_asset, target_asset, created_at DESC, id DESC', ''],
        ['swap_orders', 'idx_swap_orders_tenant_idempotency',    'tenant_id, idempotency_key', 'idempotency_key IS NOT NULL'],
        ['swap_orders', 'idx_swap_orders_tenant_external_ref',   'tenant_id, external_ref', 'external_ref IS NOT NULL'],
        ['swap_quotes', 'idx_swap_quotes_tenant_pair_created',   'tenant_id, source_asset, target_asset, created_at DESC, id DESC', ''],
        ['swap_quotes', 'idx_swap_quotes_tenant_status_created', 'tenant_id, status, created_at DESC, id DESC', ''],
        ['swap_fills',  'idx_swap_fills_tenant_order_created',   'tenant_id, order_id, created_at DESC, id DESC', ''],
        ['swap_fills',  'idx_swap_fills_tenant_created',         'tenant_id, created_at DESC, id DESC', ''],
        ['swap_fills',  'idx_swap_fills_tenant_external_ref',    'tenant_id, external_ref', 'external_ref IS NOT NULL']
    ];
    i int;
BEGIN
    FOR i IN 1 .. array_length(defs, 1) LOOP
        CONTINUE WHEN to_regclass('public.' || defs[i][1]) IS NULL;
        EXECUTE format(
            'CREATE INDEX IF NOT EXISTS %I ON %I (%s)%s',
            defs[i][2], defs[i][1], defs[i][3],
            CASE WHEN defs[i][4] = '' THEN '' ELSE ' WHERE ' || defs[i][4] END
        );
    END LOOP;
END;
$$;

SELECT ensure_swap_lookup_indexes();
