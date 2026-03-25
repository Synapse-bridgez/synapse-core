-- Migration: Add webhook replay tracking
-- This table tracks all webhook replay attempts for audit and debugging purposes

CREATE TABLE IF NOT EXISTS webhook_replay_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    transaction_id UUID NOT NULL REFERENCES transactions(id),
    replayed_by VARCHAR(255) NOT NULL DEFAULT 'admin',
    dry_run BOOLEAN NOT NULL DEFAULT false,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    replayed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Index for efficient lookups by transaction
CREATE INDEX idx_webhook_replay_history_transaction_id ON webhook_replay_history(transaction_id);

-- Index for efficient lookups by replay time
CREATE INDEX idx_webhook_replay_history_replayed_at ON webhook_replay_history(replayed_at DESC);

-- Index for filtering by success status
CREATE INDEX idx_webhook_replay_history_success ON webhook_replay_history(success);

COMMENT ON TABLE webhook_replay_history IS 'Tracks all webhook replay attempts for debugging and audit purposes';
COMMENT ON COLUMN webhook_replay_history.transaction_id IS 'Reference to the transaction being replayed';
COMMENT ON COLUMN webhook_replay_history.replayed_by IS 'User or system that initiated the replay';
COMMENT ON COLUMN webhook_replay_history.dry_run IS 'Whether this was a dry-run (test) replay';
COMMENT ON COLUMN webhook_replay_history.success IS 'Whether the replay was successful';
COMMENT ON COLUMN webhook_replay_history.error_message IS 'Error message if replay failed';
COMMENT ON COLUMN webhook_replay_history.replayed_at IS 'Timestamp when the replay was executed';
