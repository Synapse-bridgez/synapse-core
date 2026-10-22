-- #1436: Swap order execution engine.
-- swap_orders carries the lease columns used by the worker loop
-- (FOR UPDATE SKIP LOCKED + visibility timeout via lease_expires_at).
CREATE TABLE IF NOT EXISTS swap_orders (
    id               UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    status           TEXT         NOT NULL DEFAULT 'quoted',
    venue            TEXT         NOT NULL,
    source_asset     TEXT         NOT NULL,
    dest_asset       TEXT         NOT NULL,
    amount           NUMERIC      NOT NULL,
    filled_amount    NUMERIC      NOT NULL DEFAULT 0,
    attempts         INTEGER      NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    lease_owner      TEXT,
    lease_expires_at TIMESTAMPTZ,
    failure_code     TEXT,
    created_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ  NOT NULL DEFAULT NOW()
);

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
