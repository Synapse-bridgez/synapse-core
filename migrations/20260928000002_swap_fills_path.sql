-- Fill record for swaps, including the concrete path (asset hops) used on-chain
-- and the pricing plan applied, for reconciliation and best-execution reporting.
CREATE TABLE swap_fills (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL,
    venue TEXT NOT NULL,
    -- ["native", "USDC:G...", ...] intermediate hops in order
    path JSONB NOT NULL DEFAULT '[]'::jsonb,
    source_amount BIGINT NOT NULL,
    destination_amount BIGINT NOT NULL,
    pricing_plan_id UUID,
    pricing_plan_version INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_swap_fills_order ON swap_fills (order_id);
