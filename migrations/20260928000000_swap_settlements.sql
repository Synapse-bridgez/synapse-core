-- Settlement references for executed swap hand-offs (Phase 2 -> settlement).
-- One row per swap hand-off; `idempotency_key` (from SwapHandoff) makes repeated
-- settle attempts for the same hand-off resolve to the same row.
CREATE TABLE IF NOT EXISTS swap_settlements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key VARCHAR(255) NOT NULL UNIQUE,
    transaction_id UUID NOT NULL,
    tenant_id UUID,
    stellar_account VARCHAR(56) NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    amount NUMERIC NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'settlement_pending', 'settled', 'failed')),
    settlement_ref VARCHAR(255),
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Reconciliation scans the non-terminal rows after a restart.
CREATE INDEX IF NOT EXISTS idx_swap_settlements_open
    ON swap_settlements(status, created_at)
    WHERE status IN ('pending', 'settlement_pending');
