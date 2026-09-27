-- Rollback: restore the original idempotency_keys schema (key as sole PK).
ALTER TABLE idempotency_keys DROP CONSTRAINT idempotency_keys_pkey;
ALTER TABLE idempotency_keys DROP COLUMN IF EXISTS tenant_id;
ALTER TABLE idempotency_keys ADD PRIMARY KEY (key);
CREATE INDEX IF NOT EXISTS idx_idempotency_keys_key ON idempotency_keys(key);
DELETE FROM feature_flags WHERE name = 'idempotency_tenant_namespace';
