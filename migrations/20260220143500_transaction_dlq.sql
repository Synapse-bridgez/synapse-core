-- Create transaction_dlq table for failed transactions
CREATE TABLE IF NOT EXISTS transaction_dlq (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    transaction_id UUID NOT NULL,
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    anchor_transaction_id VARCHAR(255),
    error_reason TEXT NOT NULL,
    stack_trace TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    original_created_at TIMESTAMPTZ NOT NULL,
    moved_to_dlq_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_retry_at TIMESTAMPTZ
    
    -- Note: Foreign key removed due to partitioned table constraints
    -- Application-level referential integrity should be maintained
);

CREATE INDEX idx_transaction_dlq_transaction_id ON transaction_dlq(transaction_id);
CREATE INDEX idx_transaction_dlq_moved_at ON transaction_dlq(moved_to_dlq_at);
CREATE INDEX idx_transaction_dlq_stellar_account ON transaction_dlq(stellar_account);
