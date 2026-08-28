-- Safe: CONCURRENTLY builds the index without blocking writes
CREATE INDEX CONCURRENTLY idx_transactions_region ON transactions(region);
