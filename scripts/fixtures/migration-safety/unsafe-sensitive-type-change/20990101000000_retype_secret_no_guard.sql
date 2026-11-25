-- Fixture: an ALTER COLUMN ... TYPE on a security-sensitive-looking column
-- (name contains "secret") with no precondition guard against pre-existing
-- rows. This must be flagged as an ERROR by
-- scripts/check-migration-safety.sh's sensitive-column TYPE-change rule —
-- see scripts/test-migration-safety.sh, which asserts this fixture fails
-- the check.
ALTER TABLE widgets ALTER COLUMN widget_secret TYPE BYTEA USING widget_secret::bytea;
