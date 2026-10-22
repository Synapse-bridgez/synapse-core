-- #1436: Swap order execution engine.
-- swap_orders carries the lease columns used by the worker loop
-- (FOR UPDATE SKIP LOCKED + visibility timeout via lease_expires_at).
CREATE TABLE IF NOT EXISTS swap_orders (
    id               UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    account          VARCHAR(255) NOT NULL,
    status           TEXT         NOT NULL DEFAULT 'quoted',
    venue            TEXT         NOT NULL,
    source_asset     TEXT         NOT NULL,
    dest_asset       TEXT         NOT NULL,
    amount           NUMERIC      NOT NULL CHECK (amount > 0),
    filled_amount    NUMERIC      NOT NULL DEFAULT 0,
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    lease_owner      TEXT,
    lease_expires_at TIMESTAMPTZ,
    failure_code     TEXT,
    -- Quote captured at order creation
    quote_rate       NUMERIC,
    quote_expires_at TIMESTAMPTZ,
    -- Execution details (set once the order is executed)
    executed_amount  NUMERIC,
    executed_rate    NUMERIC,
    executed_at      TIMESTAMPTZ,
    -- Settlement details (set once the order is settled)
    settlement_id    UUID,
    settled_at       TIMESTAMPTZ,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_orders_account_created
    ON swap_orders (account, created_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_swap_orders_open_asset
    ON swap_orders (source_asset) WHERE status = 'open';

CREATE INDEX IF NOT EXISTS idx_swap_orders_executable
    ON swap_orders (next_attempt_at)
    WHERE status IN ('quoted', 'executing', 'partially_filled', 'reconciling');

-- One row per venue fill. The unique key makes re-recording after a crash a no-op.
CREATE TABLE IF NOT EXISTS swap_fills (
    id            UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id      UUID         NOT NULL REFERENCES swap_orders(id),
    venue_fill_id TEXT         NOT NULL,
    amount        NUMERIC      NOT NULL,
    price         NUMERIC      NOT NULL,
    created_at    TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    UNIQUE (order_id, venue_fill_id)
);

-- Append-only lifecycle event log for each execution attempt.
CREATE TABLE IF NOT EXISTS swap_order_events (
    id         BIGSERIAL    PRIMARY KEY,
    order_id   UUID         NOT NULL REFERENCES swap_orders(id),
    attempt    INTEGER      NOT NULL,
    event_type TEXT         NOT NULL,
    detail     JSONB,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_order_events_order ON swap_order_events (order_id, id);
