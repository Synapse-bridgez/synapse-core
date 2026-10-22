-- Create transactions table
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Additional fields from Anchor callback payload
    anchor_transaction_id VARCHAR(255),  -- the 'id' field from callback
    callback_type VARCHAR(20),           -- 'deposit' or 'withdrawal'
    callback_status VARCHAR(20)           -- original status from callback
);

-- Create index for faster queries
CREATE INDEX idx_transactions_status ON transactions(status);
CREATE INDEX idx_transactions_stellar_account ON transactions(stellar_account);

-- ============================================================================
-- Cross-chain bridge handoff (#1464)
-- ============================================================================

-- Handoff records produced exactly once per settled swap. The idempotency key
-- is unique so duplicate settle events cannot create a second handoff.
CREATE TABLE IF NOT EXISTS bridge_handoffs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Correlation key linking back to the swap lifecycle.
    correlation_id VARCHAR(255) NOT NULL,
    -- Idempotency key guaranteeing exactly-once handoff production.
    idempotency_key VARCHAR(255) NOT NULL,
    -- Reference to the settlement that triggered this handoff.
    settlement_reference VARCHAR(255) NOT NULL,
    -- Source asset/amount being bridged.
    source_asset VARCHAR(64) NOT NULL,
    source_amount NUMERIC NOT NULL,
    -- Destination chain and address.
    destination_chain VARCHAR(64) NOT NULL,
    destination_address VARCHAR(255) NOT NULL,
    -- Versioned canonical payload (same style as other swap lifecycle events).
    payload_version INTEGER NOT NULL DEFAULT 1,
    payload JSONB NOT NULL,
    -- Handoff status: pending, acknowledged, failed.
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    -- Acknowledgement tracking.
    acknowledged_at TIMESTAMPTZ,
    last_attempt_at TIMESTAMPTZ,
    -- Retry/replay bookkeeping.
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT bridge_handoffs_status_check
        CHECK (status IN ('pending', 'acknowledged', 'failed'))
);

-- Exactly-once guarantee: one handoff per idempotency key.
CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_handoffs_idempotency_key
    ON bridge_handoffs(idempotency_key);
CREATE UNIQUE INDEX IF NOT EXISTS idx_bridge_handoffs_settlement_reference
    ON bridge_handoffs(settlement_reference);
CREATE INDEX IF NOT EXISTS idx_bridge_handoffs_correlation_id
    ON bridge_handoffs(correlation_id);
CREATE INDEX IF NOT EXISTS idx_bridge_handoffs_status
    ON bridge_handoffs(status);
-- Supports retry/replay scans for failed handoffs.
CREATE INDEX IF NOT EXISTS idx_bridge_handoffs_status_created_at
    ON bridge_handoffs(status, created_at);

-- ============================================================================
-- Swap read-model projection (#1468)
-- ============================================================================

-- Tracks the projection schema version; a version bump forces an explicit
-- rebuild of the read model.
CREATE TABLE IF NOT EXISTS swap_projection_version (
    id INTEGER PRIMARY KEY DEFAULT 1,
    version INTEGER NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT swap_projection_version_singleton CHECK (id = 1)
);

INSERT INTO swap_projection_version (id, version)
VALUES (1, 1)
ON CONFLICT (id) DO NOTHING;

-- Checkpoint per projection stream so the projector can catch up after
-- downtime without reprocessing or skipping events.
CREATE TABLE IF NOT EXISTS swap_projection_checkpoints (
    projection_name VARCHAR(128) NOT NULL,
    aggregate_id VARCHAR(255) NOT NULL,
    last_event_id VARCHAR(255) NOT NULL,
    last_event_sequence BIGINT NOT NULL DEFAULT 0,
    last_event_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (projection_name, aggregate_id)
);

CREATE INDEX IF NOT EXISTS idx_swap_projection_checkpoints_updated_at
    ON swap_projection_checkpoints(projection_name, updated_at);

-- Denormalized order summary, one row per swap order.
CREATE TABLE IF NOT EXISTS swap_order_summary (
    order_id UUID PRIMARY KEY,
    tenant_id VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL,
    source_asset VARCHAR(64) NOT NULL,
    destination_asset VARCHAR(64) NOT NULL,
    -- Canonical asset pair key (e.g. "USDC/XLM") for filtering.
    asset_pair VARCHAR(160) NOT NULL,
    source_amount NUMERIC NOT NULL DEFAULT 0,
    filled_amount NUMERIC NOT NULL DEFAULT 0,
    destination_amount NUMERIC NOT NULL DEFAULT 0,
    quote_id UUID,
    fill_count INTEGER NOT NULL DEFAULT 0,
    -- Last applied event sequence for idempotent/ordered projection.
    last_event_sequence BIGINT NOT NULL DEFAULT 0,
    last_event_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tenant + status + time-window listing.
CREATE INDEX IF NOT EXISTS idx_swap_order_summary_tenant_status_created
    ON swap_order_summary(tenant_id, status, created_at DESC);
-- Tenant + asset pair filtering.
CREATE INDEX IF NOT EXISTS idx_swap_order_summary_tenant_asset_pair
    ON swap_order_summary(tenant_id, asset_pair, created_at DESC);
-- Tenant + time-window queries.
CREATE INDEX IF NOT EXISTS idx_swap_order_summary_tenant_created
    ON swap_order_summary(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_order_summary_status
    ON swap_order_summary(status);

-- Denormalized fill summary, one row per fill.
CREATE TABLE IF NOT EXISTS swap_fill_summary (
    fill_id UUID PRIMARY KEY,
    order_id UUID NOT NULL,
    tenant_id VARCHAR(255) NOT NULL,
    asset_pair VARCHAR(160) NOT NULL,
    fill_amount NUMERIC NOT NULL DEFAULT 0,
    fill_price NUMERIC,
    status VARCHAR(32) NOT NULL,
    filled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_event_sequence BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Tenant + order lookups.
CREATE INDEX IF NOT EXISTS idx_swap_fill_summary_tenant_order
    ON swap_fill_summary(tenant_id, order_id);
-- Tenant + asset pair + time-window fill queries.
CREATE INDEX IF NOT EXISTS idx_swap_fill_summary_tenant_asset_pair_filled
    ON swap_fill_summary(tenant_id, asset_pair, filled_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_fill_summary_tenant_filled
    ON swap_fill_summary(tenant_id, filled_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_fill_summary_order
    ON swap_fill_summary(order_id);

-- Per-tenant aggregates for dashboards.
CREATE TABLE IF NOT EXISTS swap_tenant_aggregates (
    tenant_id VARCHAR(255) PRIMARY KEY,
    total_orders BIGINT NOT NULL DEFAULT 0,
    open_orders BIGINT NOT NULL DEFAULT 0,
    completed_orders BIGINT NOT NULL DEFAULT 0,
    cancelled_orders BIGINT NOT NULL DEFAULT 0,
    failed_orders BIGINT NOT NULL DEFAULT 0,
    total_fills BIGINT NOT NULL DEFAULT 0,
    total_source_amount NUMERIC NOT NULL DEFAULT 0,
    total_filled_amount NUMERIC NOT NULL DEFAULT 0,
    last_event_sequence BIGINT NOT NULL DEFAULT 0,
    last_event_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_tenant_aggregates_updated_at
    ON swap_tenant_aggregates(updated_at DESC);