-- Safe: NOT NULL column backed by a DEFAULT avoids table lock
ALTER TABLE transactions ADD COLUMN region VARCHAR(50) NOT NULL DEFAULT 'us-east-1';
