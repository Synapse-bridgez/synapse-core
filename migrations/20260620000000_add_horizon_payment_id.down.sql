-- Rollback: Remove horizon_payment_id column and index
DROP INDEX IF EXISTS idx_transactions_horizon_payment_id;
ALTER TABLE transactions DROP COLUMN IF NOT EXISTS horizon_payment_id;
