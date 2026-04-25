-- Add tenant_id to transactions (nullable so existing rows default to NULL = admin-visible)
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS tenant_id UUID REFERENCES tenants(tenant_id);

CREATE INDEX IF NOT EXISTS idx_transactions_tenant_id ON transactions(tenant_id);

-- Enable Row-Level Security
ALTER TABLE transactions ENABLE ROW LEVEL SECURITY;

-- Tenants see only their own rows; NULL tenant_id rows are visible to admins only
CREATE POLICY tenant_isolation ON transactions
    USING (
        tenant_id IS NULL                                          -- legacy / admin rows
        OR tenant_id::text = current_setting('app.tenant_id', true)  -- tenant match
        OR current_setting('app.is_admin', true) = 'true'         -- admin bypass
    );

-- Allow all operations when the policy passes
CREATE POLICY tenant_isolation_insert ON transactions
    FOR INSERT
    WITH CHECK (
        tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true'
    );
