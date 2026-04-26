DROP POLICY IF EXISTS tenant_isolation_insert ON transactions;
DROP POLICY IF EXISTS tenant_isolation ON transactions;
ALTER TABLE transactions DISABLE ROW LEVEL SECURITY;
DROP INDEX IF EXISTS idx_transactions_tenant_id;
ALTER TABLE transactions DROP COLUMN IF EXISTS tenant_id;
