-- Fixture: ALTER TABLE ... RENAME TO breaks any running app instance, view,
-- or trigger that references the old table name immediately.
-- This must be flagged by check-migration-safety.sh — see
-- scripts/test-migration-safety.sh which asserts this fixture fails.
ALTER TABLE transactions RENAME TO ledger_entries;
