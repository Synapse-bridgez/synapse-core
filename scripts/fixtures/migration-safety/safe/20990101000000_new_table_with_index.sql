-- Fixture: a CREATE INDEX against a table created earlier in this same
-- migration. No existing writers can be blocked by a lock on a table that
-- doesn't exist yet, so this must NOT be flagged. See
-- scripts/test-migration-safety.sh, which asserts this fixture passes.
CREATE TABLE fixture_widgets (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL
);

CREATE INDEX idx_fixture_widgets_name ON fixture_widgets(name);
