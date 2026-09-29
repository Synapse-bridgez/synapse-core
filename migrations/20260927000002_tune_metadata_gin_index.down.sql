-- migrate:no_transaction

DROP INDEX CONCURRENTLY IF EXISTS idx_transactions_metadata_gin_path;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_metadata_gin
    ON transactions
    USING GIN (metadata);
