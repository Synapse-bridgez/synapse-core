-- Revert settlement_legs migration
DROP INDEX IF EXISTS idx_settlements_parent_id;
DROP INDEX IF EXISTS idx_settlement_legs_status;
DROP INDEX IF EXISTS idx_settlement_legs_settlement_id;
DROP TABLE IF EXISTS settlement_legs;
ALTER TABLE settlements DROP COLUMN IF EXISTS destination_account;
ALTER TABLE settlements DROP COLUMN IF EXISTS parent_settlement_id;
