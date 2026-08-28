-- Fixture: ADD COLUMN with NOT NULL and a DEFAULT value.
-- Existing rows receive the default; new INSERTs that omit the column also
-- get the default, so old app instances continue to work.
-- This must NOT be flagged — see scripts/test-migration-safety.sh which
-- asserts this fixture passes.
ALTER TABLE transactions ADD COLUMN region VARCHAR(50) NOT NULL DEFAULT 'us-east-1';
