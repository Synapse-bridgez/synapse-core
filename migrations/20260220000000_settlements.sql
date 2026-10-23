-- Create settlements table
CREATE TABLE IF NOT EXISTS settlements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_code VARCHAR(12) NOT NULL,
    total_amount NUMERIC NOT NULL,
    tx_count INTEGER NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- Add settlement_id to transactions table
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS settlement_id UUID REFERENCES settlements(id);
-- Create index for faster settlement lookups
CREATE INDEX IF NOT EXISTS idx_transactions_settlement_id ON transactions(settlement_id);