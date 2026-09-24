-- Create settlement_legs table to support split settlements
CREATE TABLE IF NOT EXISTS settlement_legs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    settlement_id UUID NOT NULL REFERENCES settlements(id) ON DELETE CASCADE,
    destination_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    split_type VARCHAR(20) NOT NULL DEFAULT 'fixed', -- 'fixed' or 'percentage'
    split_value NUMERIC, -- percentage (0-100) or amount for fixed splits
    sequence_order INTEGER NOT NULL DEFAULT 0,
    status VARCHAR(20) NOT NULL DEFAULT 'pending', -- pending, delivering, delivered, failed
    delivery_attempt_count INTEGER NOT NULL DEFAULT 0,
    last_delivery_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Add parent_settlement_id to settlements to support parent-child relationships
ALTER TABLE settlements
ADD COLUMN IF NOT EXISTS parent_settlement_id UUID REFERENCES settlements(id) ON DELETE SET NULL;

-- Add destination account column to settlements (for backward compatibility with non-split settlements)
ALTER TABLE settlements
ADD COLUMN IF NOT EXISTS destination_account VARCHAR(56);

-- Create indexes for efficient queries
CREATE INDEX IF NOT EXISTS idx_settlement_legs_settlement_id ON settlement_legs(settlement_id);
CREATE INDEX IF NOT EXISTS idx_settlement_legs_status ON settlement_legs(status);
CREATE INDEX IF NOT EXISTS idx_settlements_parent_id ON settlements(parent_settlement_id);
