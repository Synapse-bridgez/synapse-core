DROP POLICY IF EXISTS swap_audit_log_tenant_isolation_insert ON swap_audit_log;
DROP POLICY IF EXISTS swap_audit_log_tenant_isolation ON swap_audit_log;
DROP INDEX IF EXISTS idx_swap_audit_log_tenant_seq;
DROP INDEX IF EXISTS idx_swap_audit_log_asset_pair;
DROP INDEX IF EXISTS idx_swap_audit_log_order;
DROP INDEX IF EXISTS idx_swap_audit_log_tenant_time;
DROP TABLE IF EXISTS swap_audit_log;
