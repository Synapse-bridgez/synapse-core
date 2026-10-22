-- Versioned swap pricing plans with volume tiers and per-tenant assignment.
CREATE TABLE swap_pricing_plans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    version INTEGER NOT NULL,
    effective_from TIMESTAMPTZ NOT NULL,
    effective_to TIMESTAMPTZ,
    -- [{"min_volume": "0", "fee_bps": 30, "flat_fee": "0"}, ...] sorted ascending
    tiers JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (name, version)
);

CREATE TABLE swap_tenant_plan_assignments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    plan_id UUID NOT NULL REFERENCES swap_pricing_plans(id),
    effective_from TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_swap_plan_assign_tenant ON swap_tenant_plan_assignments (tenant_id, effective_from DESC);
