-- Fixture: an ALTER COLUMN ... TYPE on a security-sensitive-looking column
-- (name contains "secret"), guarded by a RAISE EXCEPTION precondition check
-- against pre-existing rows. This must be downgraded to a ::notice (not
-- counted as an ERROR) by scripts/check-migration-safety.sh, since the
-- data-semantic risk is enforced, not just documented — see
-- scripts/test-migration-safety.sh, which asserts this fixture passes.
DO $$
DECLARE
    row_count INTEGER;
BEGIN
    SELECT count(*) INTO row_count FROM widgets;
    IF row_count > 0 THEN
        RAISE EXCEPTION 'widgets has % existing row(s); this migration assumes it is empty', row_count;
    END IF;
END
$$;

ALTER TABLE widgets ALTER COLUMN widget_secret TYPE BYTEA USING widget_secret::bytea;
