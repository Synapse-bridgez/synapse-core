-- Swap orders and their execution records (Phase 2).
--
-- An order is created `quoted` with the minimum acceptable output captured at
-- quote time. The execution engine (src/swap/execution.rs) drives it through
-- `executing` to `settled` or `failed`. `swap_executions` holds exactly one
-- record per order; the unique `idempotency_key` is what venues dedupe on.

CREATE TABLE swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    transaction_id UUID NOT NULL REFERENCES transactions(id),
    source_asset VARCHAR(12) NOT NULL,
    target_asset VARCHAR(12) NOT NULL,
    amount NUMERIC(20, 7) NOT NULL CHECK (amount > 0),
    minimum_output NUMERIC(20, 7) NOT NULL CHECK (minimum_output > 0),
    status VARCHAR(20) NOT NULL DEFAULT 'quoted'
        CHECK (status IN ('quoted', 'executing', 'settled', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_swap_orders_status_updated ON swap_orders (status, updated_at);

CREATE TABLE swap_executions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL UNIQUE REFERENCES swap_orders(id),
    idempotency_key VARCHAR(128) NOT NULL UNIQUE,
    venue VARCHAR(64) NOT NULL,
    executed_amount_in NUMERIC(20, 7),
    executed_amount_out NUMERIC(20, 7),
    fee NUMERIC(20, 7),
    venue_reference VARCHAR(256),
    attempts INTEGER NOT NULL DEFAULT 0,
    failure_reason VARCHAR(64),
    failure_detail TEXT,
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ
);
