-- Revert per-tenant idempotency TTL configuration
DROP INDEX IF EXISTS idx_tenants_idempotency_ttl;
ALTER TABLE tenants DROP COLUMN IF EXISTS idempotency_ttl_seconds;
