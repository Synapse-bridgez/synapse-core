-- Fixture: a non-concurrent CREATE INDEX on a table this migration did NOT
-- create. This must be flagged by scripts/check-migration-safety.sh's
-- CREATE INDEX rule — see scripts/test-migration-safety.sh, which asserts
-- this fixture fails the check.
CREATE INDEX idx_transactions_fixture_column ON transactions(fixture_column);
