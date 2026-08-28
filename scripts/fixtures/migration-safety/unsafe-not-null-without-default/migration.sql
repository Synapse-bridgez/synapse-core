-- Unsafe: adding a NOT NULL column with no DEFAULT causes a full table rewrite / lock
ALTER TABLE transactions ADD COLUMN region VARCHAR(50) NOT NULL;
