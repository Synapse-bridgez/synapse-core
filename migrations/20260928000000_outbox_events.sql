-- Transactional outbox for swap/transaction lifecycle events.
--
-- Rows are written in the same SQL transaction as the domain state change, so
-- an event exists if and only if the change committed. The publisher worker
-- (src/db/outbox.rs) drains unpublished rows with at-least-once delivery.

-- Per-aggregate (order) sequence counter. Never pruned: pruning outbox_events
-- must not reset an order's sequence, or consumers would see a regression.
CREATE TABLE IF NOT EXISTS outbox_sequences (
    aggregate_id UUID PRIMARY KEY,
    last_sequence BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS outbox_events (
    -- Stable consumer-side dedupe key. Assigned once at insert and reused on
    -- every publish retry.
    event_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Order the event belongs to (transactions.id).
    aggregate_id UUID NOT NULL,
    -- Monotonic per-order sequence, starting at 1.
    sequence BIGINT NOT NULL,
    event_type TEXT NOT NULL,
    version INTEGER NOT NULL,
    -- No FK to tenants: outbox rows must not block tenant deletion.
    tenant_id UUID,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    payload JSONB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error TEXT,
    published_at TIMESTAMPTZ,
    dead_lettered_at TIMESTAMPTZ,
    UNIQUE (aggregate_id, sequence)
);

-- Pending-delivery scan: unpublished, not dead-lettered rows.
CREATE INDEX IF NOT EXISTS idx_outbox_events_pending
    ON outbox_events (next_attempt_at, occurred_at)
    WHERE published_at IS NULL AND dead_lettered_at IS NULL;

-- Per-order ordering check: lowest unpublished sequence for an aggregate.
CREATE INDEX IF NOT EXISTS idx_outbox_events_aggregate_unpublished
    ON outbox_events (aggregate_id, sequence)
    WHERE published_at IS NULL;

-- Retention pruning of published rows.
CREATE INDEX IF NOT EXISTS idx_outbox_events_published_at
    ON outbox_events (published_at)
    WHERE published_at IS NOT NULL;
