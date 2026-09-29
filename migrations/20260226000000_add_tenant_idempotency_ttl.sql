-- Add per-tenant idempotency key TTL configuration for issue #1255
ALTER TABLE tenants
ADD COLUMN IF NOT EXISTS idempotency_ttl_seconds BIGINT NOT NULL DEFAULT 86400;

-- Create index for queries filtering by TTL settings (useful for administrative operations)
CREATE INDEX IF NOT EXISTS idx_tenants_idempotency_ttl ON tenants(idempotency_ttl_seconds);

-- Add comment to clarify the field
COMMENT ON COLUMN tenants.idempotency_ttl_seconds IS
'Seconds to retain idempotency keys for this tenant. Defaults to 24 hours (86400). Higher values provide better replay protection at cost of storage; lower values reduce storage growth for high-volume tenants.';
