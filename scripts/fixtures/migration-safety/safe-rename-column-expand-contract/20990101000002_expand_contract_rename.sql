-- Fixture: safe alternative to RENAME COLUMN — the expand/contract pattern.
-- Step 1 of N: add the new column as nullable (no rename, no lock risk).
-- Old app writes to stellar_account; new app writes to both.
-- This must NOT be flagged — see scripts/test-migration-safety.sh which
-- asserts this fixture passes.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS wallet_address VARCHAR(56);
UPDATE transactions SET wallet_address = stellar_account WHERE wallet_address IS NULL;
