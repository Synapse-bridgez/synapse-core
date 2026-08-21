-- Add horizon_payment_id column for idempotency tracking
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS horizon_payment_id VARCHAR(255);

-- Index for fast lookup by Horizon payment id.
-- A cross-partition UNIQUE constraint requires the partition key (created_at);
-- uniqueness is enforced at the application layer instead.
CREATE INDEX IF NOT EXISTS idx_transactions_horizon_payment_id ON transactions(horizon_payment_id)
WHERE horizon_payment_id IS NOT NULL;
