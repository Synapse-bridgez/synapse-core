-- #1089: Namespace idempotency keys per tenant
--
-- Adds a `tenant_id` column to `idempotency_keys` so that two tenants
-- supplying identical client-generated keys never collide.  The primary key
-- changes from (key) to the composite (tenant_id, key).
--
-- Migration strategy (zero-downtime rollout):
--   1. This migration runs while the old code is still live.  All existing rows
--      are back-filled to tenant_id = 'default' (the same fallback value the
--      middleware uses when the X-Tenant-Id header is absent).
--   2. The feature flag `idempotency_tenant_namespace` is set to enabled=false,
--      rollout_percentage=0.  The new code checks the flag; when it is off it
--      continues to read/write using tenant_id = 'default', so old records are
--      still served and no request is double-processed.
--   3. Operators ramp rollout_percentage to 100 (or enable globally) once they
--      are satisfied there are no in-flight records from the pre-namespace era.
--
-- Rollback: run 20260828000001_idempotency_tenant_namespace.down.sql, which
-- drops tenant_id and restores the original PRIMARY KEY on (key).

-- Step 1: add the column with a safe default so existing rows are valid
ALTER TABLE idempotency_keys
    ADD COLUMN IF NOT EXISTS tenant_id VARCHAR(255) NOT NULL DEFAULT 'default';

-- Step 2: back-fill explicit default so the column is consistent
UPDATE idempotency_keys SET tenant_id = 'default' WHERE tenant_id = 'default';

-- Step 3: drop the old single-column primary key
ALTER TABLE idempotency_keys DROP CONSTRAINT idempotency_keys_pkey;

-- Step 4: composite primary key — tenant_id + key
ALTER TABLE idempotency_keys ADD PRIMARY KEY (tenant_id, key);

-- Step 5: replace the old key-only lookup index with a composite one
DROP INDEX IF EXISTS idx_idempotency_keys_key;
CREATE INDEX idx_idempotency_keys_tenant_key ON idempotency_keys(tenant_id, key);

-- Step 6: feature flag row for the new namespacing behaviour
INSERT INTO feature_flags (name, enabled, rollout_percentage, description)
VALUES (
    'idempotency_tenant_namespace',
    false,
    0,
    'Gate per-tenant idempotency key namespacing. Ramp rollout_percentage to 100 after back-fill is confirmed stable.'
)
ON CONFLICT (name) DO NOTHING;
