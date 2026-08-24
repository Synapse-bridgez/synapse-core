-- Part A of the cross-tenant data exposure fix: settlements had no RLS
-- policy and no way to scope a read to a single tenant at all — GET
-- /settlements and /settlements/:id returned every settlement, in full, to
-- any caller (see src/handlers/settlements.rs).
--
-- Unlike transactions, settlements do not map 1:1 to a tenant: the batch
-- job that creates them (SettlementService::settle_asset,
-- src/services/settlement.rs) groups unsettled transactions by asset_code
-- only, across every tenant, so a single settlement can legitimately
-- aggregate transactions from many tenants. Adding a `tenant_id` column
-- directly on `settlements` would misrepresent that relationship (and
-- redefining settlement batching to be per-tenant is a product decision
-- out of scope for this fix — see Known gaps in the PR description).
--
-- Instead, this policy expresses the real relationship: a settlement is
-- visible to a tenant if at least one transaction belonging to that tenant
-- (or a legacy NULL-tenant_id transaction, same convention as
-- migrations/20260501000000_tenant_rls.sql) was rolled into it.
ALTER TABLE settlements ENABLE ROW LEVEL SECURITY;
ALTER TABLE settlements FORCE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation ON settlements
    USING (
        current_setting('app.is_admin', true) = 'true'
        OR EXISTS (
            SELECT 1 FROM transactions t
            WHERE t.settlement_id = settlements.id
              AND (
                  t.tenant_id IS NULL
                  OR t.tenant_id::text = current_setting('app.tenant_id', true)
              )
        )
    );
