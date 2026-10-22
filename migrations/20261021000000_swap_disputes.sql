-- Swap disputes: separate aggregate referencing an order id (never a status on the order).
CREATE TABLE IF NOT EXISTS swap_disputes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    order_id UUID NOT NULL,
    category VARCHAR(32) NOT NULL CHECK (category IN ('settled', 'failed', 'partial_fill', 'other')),
    reporter VARCHAR(255) NOT NULL,
    description TEXT NOT NULL CHECK (char_length(description) <= 4000),
    status VARCHAR(16) NOT NULL DEFAULT 'open'
        CHECK (status IN ('open', 'triaged', 'resolved', 'closed', 'rejected')),
    assignee VARCHAR(255),
    resolution TEXT CHECK (resolution IS NULL OR char_length(resolution) <= 4000),
    adjustment_ref VARCHAR(255),
    sla_deadline TIMESTAMPTZ NOT NULL DEFAULT (NOW() + INTERVAL '72 hours'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS swap_disputes_tenant_status_idx ON swap_disputes (tenant_id, status, created_at);
CREATE INDEX IF NOT EXISTS swap_disputes_order_idx ON swap_disputes (order_id);
CREATE INDEX IF NOT EXISTS swap_disputes_sla_idx ON swap_disputes (sla_deadline)
    WHERE status IN ('open', 'triaged');
