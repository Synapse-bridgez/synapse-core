-- #1460: append-only, tamper-evident audit trail for swap lifecycle actions.
--
-- Design notes:
--  * `swap_audit_log` is a dedicated table (not a reuse of `audit_logs`) so
--    the append-only grants below can be scoped to swap activity only and so
--    the hash chain is not perturbed by unrelated audit writers.
--  * `prev_hash` / `record_hash` form a per-tenant SHA-256 hash chain: each
--    row's `record_hash` covers the previous row's `record_hash` plus this
--    row's canonical fields, so any UPDATE/DELETE/insertion in the middle of
--    a tenant's chain is detectable by re-walking the chain.
--  * `tenant_id` is nullable (NULL = legacy/admin rows, same convention as
--    `transactions.tenant_id`) and is RLS-scoped exactly like transactions.
--  * Retention follows the existing `audit_logs` policy: rows older than
--    `AUDIT_LOG_RETENTION_DAYS` (default 365) are archived then deleted by
--    `AuditLogRetentionJob`; see docs/swap-audit-trail.md.

CREATE TABLE IF NOT EXISTS swap_audit_log (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Monotonic per-tenant sequence; the chain is ordered by this column.
    seq BIGINT GENERATED ALWAYS AS IDENTITY,
    tenant_id UUID REFERENCES tenants(tenant_id),
    -- Swap order / transaction this action applies to.
    order_id UUID NOT NULL,
    -- Asset pair, e.g. 'XLM/USDC' (denormalised for export filtering).
    asset_pair TEXT,
    -- Lifecycle action: state_transition, quote_created, execution_attempt,
    -- cancellation, limit_rejection, operator_intervention.
    action VARCHAR(64) NOT NULL,
    -- Free-text reason supplied by the actor (required for operator actions).
    reason TEXT,
    actor VARCHAR(255) NOT NULL DEFAULT 'system',
    before_state JSONB,
    after_state JSONB,
    -- UTC timestamp of the action itself (not of the insert).
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    prev_hash TEXT,
    record_hash TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_swap_audit_log_tenant_time
    ON swap_audit_log (tenant_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_audit_log_order
    ON swap_audit_log (order_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_audit_log_asset_pair
    ON swap_audit_log (asset_pair, occurred_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_swap_audit_log_tenant_seq
    ON swap_audit_log (tenant_id, seq);

-- Append-only at the DB level: the application role may INSERT and SELECT,
-- never UPDATE or DELETE. Retention deletes run as the schema owner (the
-- migration/ops role), which is the only role holding DELETE here.
REVOKE ALL ON swap_audit_log FROM PUBLIC;
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'synapse_app') THEN
        REVOKE UPDATE, DELETE, TRUNCATE ON swap_audit_log FROM synapse_app;
        GRANT SELECT, INSERT ON swap_audit_log TO synapse_app;
    END IF;
END
$$;

-- Tenant isolation, mirroring migrations/20260501000000_tenant_rls.sql.
ALTER TABLE swap_audit_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE swap_audit_log FORCE ROW LEVEL SECURITY;

CREATE POLICY swap_audit_log_tenant_isolation ON swap_audit_log
    USING (
        tenant_id IS NULL
        OR tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true'
    );

CREATE POLICY swap_audit_log_tenant_isolation_insert ON swap_audit_log
    FOR INSERT
    WITH CHECK (
        tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true'
    );
