ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS memo TEXT,
ADD COLUMN IF NOT EXISTS memo_type VARCHAR(10),
ADD COLUMN IF NOT EXISTS metadata JSONB;

CREATE INDEX IF NOT EXISTS idx_transactions_metadata_gin
    ON transactions
    USING GIN (metadata);
