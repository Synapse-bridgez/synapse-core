CREATE TABLE IF NOT EXISTS swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    account VARCHAR(255) NOT NULL,
    source_asset VARCHAR(12) NOT NULL,
    target_asset VARCHAR(12) NOT NULL,
    amount NUMERIC NOT NULL CHECK (amount > 0),
    status VARCHAR(20) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'executed', 'settled', 'cancelled')),
    -- Quote captured at order creation
    quote_rate NUMERIC NOT NULL,
    quote_expires_at TIMESTAMPTZ NOT NULL,
    -- Execution details (set once the order is executed)
    executed_amount NUMERIC,
    executed_rate NUMERIC,
    executed_at TIMESTAMPTZ,
    -- Settlement details (set once the order is settled)
    settlement_id UUID,
    settled_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_swap_orders_account_created
    ON swap_orders (account, created_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_swap_orders_open_asset
    ON swap_orders (source_asset) WHERE status = 'open';
