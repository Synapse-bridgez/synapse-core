-- Fixture: ADD COLUMN ... NOT NULL without a DEFAULT on an existing table.
-- Unsafe because running instances inserting rows without specifying this column will fail.
ALTER TABLE transactions ADD COLUMN status VARCHAR(20) NOT NULL;
