-- Remove database-backed idempotency fallback table
DROP INDEX IF EXISTS idx_idempotency_keys_expires_at;
DROP INDEX IF EXISTS idx_idempotency_keys_key;
DROP TABLE IF EXISTS idempotency_keys;