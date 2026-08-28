-- Unsafe: changes column type without USING clause
ALTER TABLE transactions ALTER COLUMN amount TYPE INTEGER;
