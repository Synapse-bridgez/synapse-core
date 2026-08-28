-- Fixture: RENAME COLUMN breaks any running app instance that references
-- the old column name in its SQL queries immediately upon execution.
-- This must be flagged by check-migration-safety.sh — see
-- scripts/test-migration-safety.sh which asserts this fixture fails.
ALTER TABLE transactions RENAME COLUMN stellar_account TO wallet_address;
