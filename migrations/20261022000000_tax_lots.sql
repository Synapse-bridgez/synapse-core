-- Migration: Tax lot tracking for cost basis and realized P&L — Issue #1489
--
-- Three tables:
--   tax_lots           — acquisition records (one per buy fill / incoming credit)
--   lot_consumptions   — individual lot consumption events (one per lot touched in a sell)
--   realized_pnl       — aggregated gain/loss per sell event

-- ── tax_lots ─────────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS tax_lots (
    id                  UUID        NOT NULL DEFAULT gen_random_uuid(),
    tenant_id           UUID        NOT NULL,
    asset_code          TEXT        NOT NULL,
    asset_issuer        TEXT,                     -- NULL for native XLM
    quantity            NUMERIC(28, 7) NOT NULL CHECK (quantity > 0),
    remaining           NUMERIC(28, 7) NOT NULL CHECK (remaining >= 0),
    cost_per_unit       NUMERIC(28, 7) NOT NULL CHECK (cost_per_unit >= 0),
    total_cost          NUMERIC(28, 7) NOT NULL CHECK (total_cost >= 0),
    reporting_currency  TEXT        NOT NULL DEFAULT 'USD',
    acquisition_fee     NUMERIC(28, 7) NOT NULL DEFAULT 0 CHECK (acquisition_fee >= 0),
    source_order_id     TEXT        NOT NULL,
    acquired_at         TIMESTAMPTZ NOT NULL,
    reversed            BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (id),
    -- RLS key: every query must supply tenant context
    CONSTRAINT tax_lots_tenant_fk
        FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE RESTRICT
) PARTITION BY RANGE (acquired_at);

-- Monthly partitions starting from the current month.
-- Additional partitions are managed by the existing partition maintenance job.
CREATE TABLE IF NOT EXISTS tax_lots_y2026m10
    PARTITION OF tax_lots
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');

CREATE TABLE IF NOT EXISTS tax_lots_y2026m11
    PARTITION OF tax_lots
    FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');

CREATE TABLE IF NOT EXISTS tax_lots_y2026m12
    PARTITION OF tax_lots
    FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');

-- Indexes
CREATE INDEX IF NOT EXISTS tax_lots_tenant_asset_idx
    ON tax_lots (tenant_id, asset_code, asset_issuer, acquired_at)
    WHERE NOT reversed AND remaining > 0;

CREATE INDEX IF NOT EXISTS tax_lots_source_order_idx
    ON tax_lots (source_order_id);

CREATE INDEX IF NOT EXISTS tax_lots_acquired_at_idx
    ON tax_lots (acquired_at);

-- ── lot_consumptions ──────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS lot_consumptions (
    id                  UUID        NOT NULL DEFAULT gen_random_uuid(),
    lot_id              UUID        NOT NULL,
    tenant_id           UUID        NOT NULL,
    quantity_consumed   NUMERIC(28, 7) NOT NULL CHECK (quantity_consumed > 0),
    cost_basis          NUMERIC(28, 7) NOT NULL CHECK (cost_basis >= 0),
    source_order_id     TEXT        NOT NULL,
    consumed_at         TIMESTAMPTZ NOT NULL,
    reversed            BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (id),
    CONSTRAINT lot_consumptions_lot_fk
        FOREIGN KEY (lot_id) REFERENCES tax_lots(id) ON DELETE RESTRICT,
    CONSTRAINT lot_consumptions_tenant_fk
        FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS lot_consumptions_lot_id_idx
    ON lot_consumptions (lot_id);

CREATE INDEX IF NOT EXISTS lot_consumptions_tenant_order_idx
    ON lot_consumptions (tenant_id, source_order_id);

CREATE INDEX IF NOT EXISTS lot_consumptions_consumed_at_idx
    ON lot_consumptions (consumed_at);

-- ── realized_pnl ──────────────────────────────────────────────────────────────

CREATE TABLE IF NOT EXISTS realized_pnl (
    id                  UUID        NOT NULL DEFAULT gen_random_uuid(),
    tenant_id           UUID        NOT NULL,
    asset_code          TEXT        NOT NULL,
    asset_issuer        TEXT,
    proceeds            NUMERIC(28, 7) NOT NULL,
    cost_basis          NUMERIC(28, 7) NOT NULL CHECK (cost_basis >= 0),
    gain_loss           NUMERIC(28, 7) NOT NULL,  -- can be negative (loss)
    sale_fee            NUMERIC(28, 7) NOT NULL DEFAULT 0 CHECK (sale_fee >= 0),
    lot_method          TEXT        NOT NULL CHECK (lot_method IN ('FIFO','LIFO','HIFO','SPECIFIC_LOT')),
    reporting_currency  TEXT        NOT NULL DEFAULT 'USD',
    source_order_id     TEXT        NOT NULL,
    realized_at         TIMESTAMPTZ NOT NULL,
    reversed            BOOLEAN     NOT NULL DEFAULT FALSE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (id),
    CONSTRAINT realized_pnl_tenant_fk
        FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE RESTRICT
) PARTITION BY RANGE (realized_at);

CREATE TABLE IF NOT EXISTS realized_pnl_y2026m10
    PARTITION OF realized_pnl
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');

CREATE TABLE IF NOT EXISTS realized_pnl_y2026m11
    PARTITION OF realized_pnl
    FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');

CREATE TABLE IF NOT EXISTS realized_pnl_y2026m12
    PARTITION OF realized_pnl
    FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');

CREATE INDEX IF NOT EXISTS realized_pnl_tenant_period_idx
    ON realized_pnl (tenant_id, realized_at)
    WHERE NOT reversed;

CREATE INDEX IF NOT EXISTS realized_pnl_source_order_idx
    ON realized_pnl (source_order_id);

-- ── lot_consumption_pnl_map ───────────────────────────────────────────────────
-- Many-to-many link between lot_consumptions and realized_pnl entries
-- (a single P&L entry may arise from multiple lot consumptions).

CREATE TABLE IF NOT EXISTS lot_consumption_pnl_map (
    consumption_id  UUID NOT NULL,
    pnl_id          UUID NOT NULL,
    PRIMARY KEY (consumption_id, pnl_id),
    CONSTRAINT lcpm_consumption_fk
        FOREIGN KEY (consumption_id) REFERENCES lot_consumptions(id) ON DELETE CASCADE,
    CONSTRAINT lcpm_pnl_fk
        FOREIGN KEY (pnl_id) REFERENCES realized_pnl(id) ON DELETE CASCADE
);

-- ── Row-Level Security ────────────────────────────────────────────────────────
-- Follows the same pattern as transactions/settlements (see
-- docs/postmortem-cross-tenant-leak.md): every DML uses tenant_id equality,
-- and synapse_app connects as NOBYPASSRLS.

ALTER TABLE tax_lots ENABLE ROW LEVEL SECURITY;
ALTER TABLE tax_lots FORCE ROW LEVEL SECURITY;

CREATE POLICY tax_lots_tenant_isolation ON tax_lots
    USING (tenant_id = current_setting('app.current_tenant_id')::UUID);

ALTER TABLE lot_consumptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE lot_consumptions FORCE ROW LEVEL SECURITY;

CREATE POLICY lot_consumptions_tenant_isolation ON lot_consumptions
    USING (tenant_id = current_setting('app.current_tenant_id')::UUID);

ALTER TABLE realized_pnl ENABLE ROW LEVEL SECURITY;
ALTER TABLE realized_pnl FORCE ROW LEVEL SECURITY;

CREATE POLICY realized_pnl_tenant_isolation ON realized_pnl
    USING (tenant_id = current_setting('app.current_tenant_id')::UUID);

-- ── updated_at trigger ────────────────────────────────────────────────────────

CREATE OR REPLACE FUNCTION set_tax_lots_updated_at()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$;

CREATE TRIGGER tax_lots_updated_at
    BEFORE UPDATE ON tax_lots
    FOR EACH ROW EXECUTE FUNCTION set_tax_lots_updated_at();
