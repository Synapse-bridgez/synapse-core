-- Fixture: ALTER TABLE ... RENAME TO breaks any running app instance referencing the old table name.
ALTER TABLE settlements RENAME TO settlements_archived;
