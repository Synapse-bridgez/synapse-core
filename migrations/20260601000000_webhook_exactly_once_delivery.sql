-- Add exactly-once delivery support, DLQ routing, and circuit breaker columns
-- to the webhook delivery system.

-- ── 1. New columns on webhook_deliveries ────────────────────────────────────

-- Store full attempt history as JSONB array for DLQ routing
ALTER TABLE webhook_deliveries
    ADD COLUMN IF NOT EXISTS attempt_history JSONB NOT NULL DEFAULT '[]'::jsonb;

-- Track which worker claimed a delivery (for reclaim of crashed workers)
ALTER TABLE webhook_deliveries
    ADD COLUMN IF NOT EXISTS claimed_at TIMESTAMPTZ;

-- Index for reclaim queries (stale in_progress rows)
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_claimed_at
    ON webhook_deliveries(claimed_at)
    WHERE status = 'in_progress';

-- Update the status comment: pending | in_progress | delivered | failed
COMMENT ON COLUMN webhook_deliveries.status IS
    'pending | in_progress | delivered | failed';

-- ── 2. Circuit breaker columns on webhook_endpoints ─────────────────────────

ALTER TABLE webhook_endpoints
    ADD COLUMN IF NOT EXISTS circuit_state VARCHAR(20) NOT NULL DEFAULT 'closed';
ALTER TABLE webhook_endpoints
    ADD COLUMN IF NOT EXISTS circuit_failure_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE webhook_endpoints
    ADD COLUMN IF NOT EXISTS circuit_opened_at TIMESTAMPTZ;

-- ── 3. Webhook delivery DLQ table ───────────────────────────────────────────

CREATE TABLE IF NOT EXISTS webhook_delivery_dlq (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    delivery_id         UUID NOT NULL,
    endpoint_id         UUID NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    transaction_id      UUID NOT NULL,
    event_type          TEXT NOT NULL,
    payload             JSONB NOT NULL,
    attempt_history     JSONB NOT NULL DEFAULT '[]'::jsonb,
    attempt_count       INTEGER NOT NULL,
    last_response_status INTEGER,
    last_response_body  TEXT,
    last_error          TEXT,
    moved_to_dlq_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    replayed_at         TIMESTAMPTZ,
    replay_count        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_webhook_dlq_endpoint_id
    ON webhook_delivery_dlq(endpoint_id);
CREATE INDEX IF NOT EXISTS idx_webhook_dlq_transaction_id
    ON webhook_delivery_dlq(transaction_id);
CREATE INDEX IF NOT EXISTS idx_webhook_dlq_moved_at
    ON webhook_delivery_dlq(moved_to_dlq_at DESC);
CREATE INDEX IF NOT EXISTS idx_webhook_dlq_replay_count
    ON webhook_delivery_dlq(replay_count)
    WHERE replay_count > 0;

COMMENT ON TABLE webhook_delivery_dlq IS
    'Dead-letter queue for webhook deliveries that exhausted all retry attempts';
COMMENT ON COLUMN webhook_delivery_dlq.delivery_id IS
    'Original webhook_deliveries.id (kept for traceability across re-enqueues)';
COMMENT ON COLUMN webhook_delivery_dlq.attempt_history IS
    'JSON array of {attempt, attempted_at, response_status, response_body, error} objects';
COMMENT ON COLUMN webhook_delivery_dlq.replay_count IS
    'Number of times this DLQ entry has been replayed back to the dispatcher';
