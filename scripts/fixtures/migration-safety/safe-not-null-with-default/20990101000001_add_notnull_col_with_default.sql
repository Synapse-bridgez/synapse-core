-- Fixture: ADD COLUMN ... NOT NULL with a DEFAULT on an existing table.
-- Safe because running instances omitting the column will have the default populated.
ALTER TABLE transactions ADD COLUMN status VARCHAR(20) NOT NULL DEFAULT 'pending';
