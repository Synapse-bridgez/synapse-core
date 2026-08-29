-- Fixture: ADD COLUMN with NOT NULL but no DEFAULT value.
-- Any running app instance that INSERTs without supplying this column will
-- fail immediately with a constraint violation.
-- This must be flagged by check-migration-safety.sh — see
-- scripts/test-migration-safety.sh which asserts this fixture fails.
ALTER TABLE transactions ADD COLUMN region VARCHAR(50) NOT NULL;
