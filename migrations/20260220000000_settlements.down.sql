DROP INDEX IF EXISTS idx_transactions_settlement_id;
ALTER TABLE transactions DROP COLUMN IF EXISTS settlement_id;
DROP TABLE IF EXISTS settlements;
