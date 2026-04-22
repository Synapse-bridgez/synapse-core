-- Add indexes for transaction search optimization
-- These indexes support the search endpoint's filtering capabilities

-- Index for asset_code filtering (if not already exists)
CREATE INDEX IF NOT EXISTS idx_transactions_asset_code ON transactions(asset_code);

-- Composite index for date range queries with status
CREATE INDEX IF NOT EXISTS idx_transactions_created_status ON transactions(created_at DESC, status);

-- Composite index for amount range queries
CREATE INDEX IF NOT EXISTS idx_transactions_amount ON transactions(amount);

-- Composite index for cursor-based pagination (created_at, id)
CREATE INDEX IF NOT EXISTS idx_transactions_created_id ON transactions(created_at DESC, id DESC);

-- Composite index for common search patterns: status + asset_code + created_at
CREATE INDEX IF NOT EXISTS idx_transactions_search ON transactions(status, asset_code, created_at DESC);
