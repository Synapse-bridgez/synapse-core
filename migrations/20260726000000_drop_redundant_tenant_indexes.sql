-- Drop indexes that duplicate the implicit unique index already created by
-- the UNIQUE / PRIMARY KEY constraints on these columns. Postgres does not
-- dedupe these automatically, so both were being maintained independently
-- on every write, doubling write-amplification on hot paths.
DROP INDEX IF EXISTS idx_tenants_api_key;
DROP INDEX IF EXISTS idx_idempotency_keys_key;
