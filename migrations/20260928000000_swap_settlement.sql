-- Swap orders, fills and event log with settlement tracking.
CREATE TABLE IF NOT EXISTS swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    status TEXT NOT NULL DEFAULT 'executed'
        CHECK (status IN ('executed', 'settlement_pending', 'settled', 'failed')),
    destination_account TEXT NOT NULL,
    output_asset TEXT NOT NULL,
    output_amount NUMERIC NOT NULL,
    settlement_attempt INT NOT NULL DEFAULT 0,
    settlement_idempotency_key TEXT,
    settlement_tx_hash TEXT,
    settlement_external_status TEXT,
    failure_reason_code TEXT,
    settlement_started_at TIMESTAMPTZ,
    settled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS swap_fills (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES swap_orders(id),
    settlement_tx_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS swap_events (
    id BIGSERIAL PRIMARY KEY,
    order_id UUID NOT NULL REFERENCES swap_orders(id),
    event_type TEXT NOT NULL,
    attempt INT,
    external_reference TEXT,
    raw_status TEXT,
    detail JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_orders_pending
    ON swap_orders (settlement_started_at) WHERE status = 'settlement_pending';
CREATE INDEX IF NOT EXISTS idx_swap_events_order ON swap_events (order_id, id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_swap_orders_settlement_key
    ON swap_orders (settlement_idempotency_key) WHERE settlement_idempotency_key IS NOT NULL;
