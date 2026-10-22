-- #1418: Swap order domain model and persisted lifecycle
--
-- Durable Phase 2 swap intent. Monthly range-partitioned by created_at, the
-- same convention as `transactions` (see docs/partitioning.md), so the primary
-- key includes created_at.
--
-- The lifecycle is enforced in Rust (SwapOrderStatus::can_transition_to); the
-- CHECK on status is only a backstop against unknown values.
-- `version` is the optimistic-concurrency token: every transition is a
-- compare-and-set on (id, created_at, version).

CREATE TABLE IF NOT EXISTS swap_orders (
    id                UUID            NOT NULL DEFAULT gen_random_uuid(),
    tenant_id         UUID            NOT NULL REFERENCES tenants(tenant_id),
    stellar_account   VARCHAR(56)     NOT NULL,
    source_asset      VARCHAR(12)     NOT NULL,
    destination_asset VARCHAR(12)     NOT NULL,
    -- Fixed-point, never float. Stellar amounts carry 7 decimal places.
    amount            NUMERIC(28, 7)  NOT NULL CHECK (amount > 0),
    quoted_amount     NUMERIC(28, 7),
    quote_id          VARCHAR(255),
    status            VARCHAR(32)     NOT NULL DEFAULT 'swap_ready'
        CHECK (status IN ('swap_ready', 'quoting', 'quoted', 'executing', 'settled', 'failed', 'cancelled')),
    version           BIGINT          NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ     NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ     NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);

CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_created ON swap_orders(tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_swap_orders_status ON swap_orders(status);

-- Initial monthly partitions (current month + next 2), matching the
-- transactions convention. Scheduled partition maintenance does not extend
-- swap_orders yet.
CREATE TABLE IF NOT EXISTS swap_orders_y2026m10 PARTITION OF swap_orders
    FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');

CREATE TABLE IF NOT EXISTS swap_orders_y2026m11 PARTITION OF swap_orders
    FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');

CREATE TABLE IF NOT EXISTS swap_orders_y2026m12 PARTITION OF swap_orders
    FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');

COMMENT ON TABLE swap_orders IS
    'Phase 2 swap orders. Lifecycle: swap_ready -> quoting -> quoted -> executing -> settled, '
    'with failed/cancelled as terminal exits. Transitions are enforced by SwapOrderStatus '
    'and applied as compare-and-set on version.';
