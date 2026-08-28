-- Safe: type change with explicit USING clause
ALTER TABLE transactions ALTER COLUMN amount TYPE BIGINT USING amount::BIGINT;
