-- Recreate the redundant indexes dropped in the up migration.
CREATE INDEX IF NOT EXISTS idx_tenants_api_key ON tenants(api_key);
CREATE INDEX idx_idempotency_keys_key ON idempotency_keys(key);
