-- Create transactions table
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Additional fields from Anchor callback payload
    anchor_transaction_id VARCHAR(255),  -- the 'id' field from callback
    callback_type VARCHAR(20),           -- 'deposit' or 'withdrawal'
    callback_status VARCHAR(20)           -- original status from callback
);

-- Create index for faster queries
CREATE INDEX idx_transactions_status ON transactions(status);
CREATE INDEX idx_transactions_stellar_account ON transactions(stellar_account);
