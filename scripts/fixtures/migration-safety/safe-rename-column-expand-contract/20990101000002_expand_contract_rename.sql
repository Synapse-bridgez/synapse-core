-- Fixture: expand/contract pattern — add new column nullable and backfill instead of immediate rename.
ALTER TABLE transactions ADD COLUMN IF NOT EXISTS description TEXT;
