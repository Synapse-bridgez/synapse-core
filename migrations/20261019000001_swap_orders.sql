-- #1440: REST API for swap quotes and orders.
--
-- Every row is owned by a tenant; all API queries filter on tenant_id.

CREATE TABLE IF NOT EXISTS swap_quotes (
    id            UUID PRIMARY KEY,
    tenant_id     UUID NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    source_asset  TEXT NOT NULL,
    dest_asset    TEXT NOT NULL,
    source_amount NUMERIC(30, 7) NOT NULL CHECK (source_amount > 0),
    dest_amount   NUMERIC(30, 7) NOT NULL CHECK (dest_amount > 0),
    rate          NUMERIC(30, 12) NOT NULL,
    fee_bps       INTEGER NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_quotes_tenant_created
    ON swap_quotes (tenant_id, created_at DESC, id DESC);

CREATE TABLE IF NOT EXISTS swap_orders (
    id              UUID PRIMARY KEY,
    tenant_id       UUID NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    quote_id        UUID NOT NULL UNIQUE REFERENCES swap_quotes(id),
    source_asset    TEXT NOT NULL,
    dest_asset      TEXT NOT NULL,
    source_amount   NUMERIC(30, 7) NOT NULL,
    dest_amount     NUMERIC(30, 7) NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'executing', 'completed', 'failed', 'canceled')),
    idempotency_key TEXT NOT NULL,
    request_hash    TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (tenant_id, idempotency_key)
);

-- Keyset pagination: (tenant_id, created_at DESC, id DESC) serves the list
-- query (with or without a status filter) as an index range scan.
CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_created
    ON swap_orders (tenant_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_status_created
    ON swap_orders (tenant_id, status, created_at DESC, id DESC);
