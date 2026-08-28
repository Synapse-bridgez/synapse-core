-- Unsafe: renaming a column will break any application code still referencing the old name
ALTER TABLE transactions RENAME COLUMN stellar_account TO wallet_address;
