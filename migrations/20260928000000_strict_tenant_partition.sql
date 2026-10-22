-- Make tenant partitioning strict for transactions and settlements.
--
-- The original policies (migrations/20260501000000_tenant_rls.sql and
-- migrations/20260824000001_settlement_rls.sql) treated `tenant_id IS NULL`
-- as "legacy / admin-visible" but also let it match every tenant's policy
-- check, so every NULL-tenant row was readable by every tenant. Callback
-- ingestion (persist_transaction in src/db/queries.rs) never writes
-- tenant_id, so every new transaction is NULL and was visible cross-tenant.
--
-- NULL rows are now visible only to the admin context (app.is_admin = 'true').

DROP POLICY IF EXISTS tenant_isolation ON transactions;

CREATE POLICY tenant_isolation ON transactions
    USING (
        tenant_id::text = current_setting('app.tenant_id', true)   -- tenant match
        OR current_setting('app.is_admin', true) = 'true'          -- admin bypass
    );

DROP POLICY IF EXISTS tenant_isolation ON settlements;

CREATE POLICY tenant_isolation ON settlements
    USING (
        current_setting('app.is_admin', true) = 'true'
        OR EXISTS (
            SELECT 1 FROM transactions t
            WHERE t.settlement_id = settlements.id
              AND t.tenant_id::text = current_setting('app.tenant_id', true)
        )
    );
