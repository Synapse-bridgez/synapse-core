-- Add horizon_payment_id column for idempotency tracking
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS horizon_payment_id VARCHAR(255);

-- Create unique index to enforce at most one transaction per Horizon payment.
-- `transactions` is PARTITION BY RANGE (created_at); Postgres requires every
-- unique index on a partitioned table to include the partition key, so
-- `created_at` is included here. This enforces uniqueness of
-- (horizon_payment_id, created_at) rather than a strictly global uniqueness
-- of horizon_payment_id across all partitions/created_at values.
CREATE UNIQUE INDEX IF NOT EXISTS idx_transactions_horizon_payment_id ON transactions(horizon_payment_id, created_at)
WHERE horizon_payment_id IS NOT NULL;
