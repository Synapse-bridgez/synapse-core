-- Unsafe: renaming a table will break application code referencing the old name
ALTER TABLE transactions RENAME TO ledger_entries;
