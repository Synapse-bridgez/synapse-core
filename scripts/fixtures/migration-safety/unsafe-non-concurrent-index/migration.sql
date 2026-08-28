-- Unsafe: CREATE INDEX without CONCURRENTLY takes an exclusive write lock
CREATE INDEX idx_transactions_region ON transactions(region);
