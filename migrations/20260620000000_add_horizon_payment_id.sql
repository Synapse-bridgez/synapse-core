-- Add horizon_payment_id column for idempotency tracking
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS horizon_payment_id VARCHAR(255);

-- Create unique index to enforce at most one transaction per Horizon payment
CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_horizon_payment_id ON transactions(horizon_payment_id)
WHERE horizon_payment_id IS NOT NULL;
