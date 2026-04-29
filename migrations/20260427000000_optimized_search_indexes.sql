-- Composite index aligned with search_transactions ORDER BY clause:
-- supports (status, asset_code) filters + created_at DESC ordering in one scan.
CREATE INDEX IF NOT EXISTS idx_transactions_status_asset_created
    ON transactions (status, asset_code, created_at DESC);

-- Partial index for processor queries that only touch pending rows.
CREATE INDEX IF NOT EXISTS idx_transactions_pending
    ON transactions (created_at DESC)
    WHERE status = 'pending';

-- GIN index for JSON path queries on the metadata column.
CREATE INDEX IF NOT EXISTS idx_transactions_metadata_gin
    ON transactions USING GIN (metadata jsonb_path_ops)
    WHERE metadata IS NOT NULL;
