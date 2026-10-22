-- #1505/#1506: normalized swap activity feed. Source of truth for swap
-- analytics rollups and trade-surveillance rules. Written from swap lifecycle
-- events; read-only for both consumers.
CREATE TABLE IF NOT EXISTS swap_activity (
    id                   UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            UUID        NOT NULL,
    account              TEXT        NOT NULL,
    pair                 TEXT        NOT NULL,
    route                TEXT        NOT NULL DEFAULT 'default',
    direction            TEXT        NOT NULL CHECK (direction IN ('buy', 'sell')),
    kind                 TEXT        NOT NULL CHECK (kind IN ('quote', 'order')),
    status               TEXT        NOT NULL CHECK (status IN ('quoted', 'filled', 'failed', 'rejected')),
    amount               NUMERIC     NOT NULL DEFAULT 0,
    price                NUMERIC,
    ref_price            NUMERIC,
    slippage_bps         NUMERIC,
    settlement_ms        BIGINT,
    counterparty_tenant_id UUID,
    owner_group          TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_swap_activity_tenant_time ON swap_activity (tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_swap_activity_time ON swap_activity (created_at);
