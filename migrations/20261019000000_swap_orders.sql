-- #1423: Swap order persistence for the REST surface (POST/GET /swaps).
--
-- An order is created either from a completed deposit transaction
-- (deposit_transaction_id) or from an explicit request. Lifecycle:
-- pending -> routed -> completed | failed, with pending -> cancelled allowed
-- via POST /swaps/:id/cancel. quote and execution are populated by the Phase 2
-- swap engine; they stay NULL until it exists.

CREATE TABLE IF NOT EXISTS swap_orders (
    id                      UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id               UUID        NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    -- Client-supplied key; a retried create with the same key returns the original order.
    idempotency_key         TEXT        NOT NULL CHECK (char_length(idempotency_key) BETWEEN 1 AND 128),
    -- Plain UUID, not a FK: transactions is partitioned and cannot be a FK target.
    deposit_transaction_id  UUID,
    source_asset            TEXT        NOT NULL,
    target_asset            TEXT        NOT NULL,
    amount                  NUMERIC     NOT NULL CHECK (amount > 0),
    status                  TEXT        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'routed', 'completed', 'failed', 'cancelled')),
    quote                   JSONB,
    execution               JSONB,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT swap_orders_tenant_idempotency_key UNIQUE (tenant_id, idempotency_key)
);

-- A completed deposit can back at most one swap order per tenant.
CREATE UNIQUE INDEX IF NOT EXISTS idx_swap_orders_tenant_deposit
    ON swap_orders(tenant_id, deposit_transaction_id)
    WHERE deposit_transaction_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_created
    ON swap_orders(tenant_id, created_at DESC, id DESC);
