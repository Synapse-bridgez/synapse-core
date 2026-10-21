-- Conditional orders, asset lifecycle status, fill TCA, and price provider health.

CREATE TABLE IF NOT EXISTS swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID,
    source_asset TEXT NOT NULL,
    dest_asset TEXT NOT NULL,
    order_type TEXT NOT NULL DEFAULT 'market'
        CHECK (order_type IN ('market', 'limit', 'stop_loss', 'take_profit')),
    status TEXT NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'triggered', 'filled', 'cancelled', 'expired')),
    trigger_price NUMERIC,
    trigger_direction TEXT CHECK (trigger_direction IN ('above', 'below')),
    limit_price NUMERIC,
    time_in_force TEXT NOT NULL DEFAULT 'gtc' CHECK (time_in_force IN ('gtc', 'ioc', 'gtd')),
    expires_at TIMESTAMPTZ,
    triggered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_swap_orders_status_trigger ON swap_orders (status, trigger_price);

CREATE TABLE IF NOT EXISTS asset_status (
    target TEXT PRIMARY KEY, -- asset code or "SRC/DST" pair
    status TEXT NOT NULL DEFAULT 'ACTIVE'
        CHECK (status IN ('ACTIVE', 'SUSPENDED', 'FROZEN', 'DELISTED')),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Append-only audit of administrative actions.
CREATE TABLE IF NOT EXISTS asset_admin_actions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    action TEXT NOT NULL
        CHECK (action IN ('suspend_pair', 'resume_pair', 'freeze_asset', 'delist_asset')),
    target TEXT NOT NULL,
    actor TEXT NOT NULL,
    reason TEXT NOT NULL,
    effective_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_asset_admin_actions_target ON asset_admin_actions (target, created_at DESC);

-- Raw TCA inputs; metrics are derived at query time.
CREATE TABLE IF NOT EXISTS swap_fill_tca (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL,
    tenant_id UUID,
    asset_pair TEXT NOT NULL,
    route TEXT,
    liquidity_provider TEXT,
    side TEXT NOT NULL CHECK (side IN ('buy', 'sell')),
    reference_price NUMERIC,
    reference_source TEXT,
    reference_at TIMESTAMPTZ,
    decision_price NUMERIC,
    fill_price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    lp_fee NUMERIC NOT NULL DEFAULT 0,
    network_fee NUMERIC NOT NULL DEFAULT 0,
    post_fill_price NUMERIC,
    filled_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_swap_fill_tca_tenant_pair ON swap_fill_tca (tenant_id, asset_pair, filled_at DESC);

CREATE TABLE IF NOT EXISTS price_provider_health (
    asset_pair TEXT NOT NULL,
    provider TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'healthy' CHECK (state IN ('healthy', 'quarantined')),
    clean_streak INTEGER NOT NULL DEFAULT 0,
    last_anomalies TEXT[] NOT NULL DEFAULT '{}',
    override_actor TEXT,
    override_reason TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (asset_pair, provider)
);
