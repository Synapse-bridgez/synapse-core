-- Fixture: safe alternative to table rename — create a new table and leave
-- a view under the old name so old app instances continue to work.
-- No RENAME TO; no RENAME COLUMN. This must NOT be flagged — see
-- scripts/test-migration-safety.sh which asserts this fixture passes.
CREATE TABLE IF NOT EXISTS ledger_entries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- Old app code targeting "transactions" keeps working via this view.
CREATE OR REPLACE VIEW transactions_view AS SELECT * FROM ledger_entries;
