-- Idempotency guard for anchor platform callbacks.
--
-- The transactions table is partitioned by created_at, so PostgreSQL cannot
-- enforce a cross-partition unique constraint on anchor_transaction_id alone.
-- This table acts as the uniqueness arbiter: whichever delivery inserts here
-- first owns the anchor_transaction_id; all later deliveries of the same key
-- are redirected to the existing transaction row.
CREATE TABLE IF NOT EXISTS anchor_transaction_dedup (
    anchor_transaction_id TEXT        NOT NULL,
    transaction_id        UUID        NOT NULL,
    transaction_created_at TIMESTAMPTZ NOT NULL,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT anchor_transaction_dedup_pkey PRIMARY KEY (anchor_transaction_id)
);

COMMENT ON TABLE anchor_transaction_dedup IS
    'Cross-partition uniqueness guard for anchor_transaction_id. '
    'One row per inbound anchor callback key; maps to the canonical transaction row.';

COMMENT ON COLUMN anchor_transaction_dedup.transaction_id IS
    'UUID of the winning transaction row in the partitioned transactions table.';

COMMENT ON COLUMN anchor_transaction_dedup.transaction_created_at IS
    'created_at of the winning row - needed to locate it on the correct partition.';
