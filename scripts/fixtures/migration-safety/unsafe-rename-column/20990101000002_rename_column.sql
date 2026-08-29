-- Fixture: RENAME COLUMN breaks any running app instance querying the old column name.
ALTER TABLE transactions RENAME COLUMN memo TO description;
