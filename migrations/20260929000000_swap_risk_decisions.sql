-- Per-order decisions from the swap risk gate (src/swap/risk.rs).
-- `accepted` and `approved` rows count against tenant exposure and velocity;
-- `held` rows wait for a manual approval before they can be routed.
CREATE TABLE IF NOT EXISTS swap_risk_decisions (
    transaction_id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    amount NUMERIC NOT NULL,
    decision VARCHAR(16) NOT NULL CHECK (decision IN ('accepted', 'held', 'approved')),
    breach JSONB,
    decided_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    approved_by VARCHAR(255),
    approved_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_swap_risk_decisions_tenant_decided
    ON swap_risk_decisions (tenant_id, decided_at);
