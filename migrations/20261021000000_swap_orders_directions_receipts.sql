-- Swap orders/quotes with explicit direction (#1502), immutable receipts (#1513),
-- encrypted sensitive columns (#1512) and the console audit log (#1509).
CREATE TABLE IF NOT EXISTS swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL,
    direction TEXT NOT NULL DEFAULT 'fiat_to_stellar'
        CHECK (direction IN ('fiat_to_stellar', 'stellar_to_fiat')),
    status TEXT NOT NULL,
    source_asset TEXT NOT NULL,
    dest_asset TEXT NOT NULL,
    amount NUMERIC NOT NULL,
    -- expand phase: encrypted envelope columns (v1.<ver>.<dek>.<nonce>.<ct>)
    counterparty_account_enc TEXT,
    memo_enc TEXT,
    kyc_ref_enc TEXT,
    enc_key_version INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_swap_orders_tenant_direction ON swap_orders (tenant_id, direction, status);
CREATE INDEX IF NOT EXISTS idx_swap_orders_enc_key_version ON swap_orders (enc_key_version);

CREATE TABLE IF NOT EXISTS swap_quotes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID REFERENCES swap_orders(id),
    direction TEXT NOT NULL DEFAULT 'fiat_to_stellar'
        CHECK (direction IN ('fiat_to_stellar', 'stellar_to_fiat')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Anchor retries callbacks: (order_id, callback_id) uniqueness gives idempotency.
CREATE TABLE IF NOT EXISTS off_ramp_callbacks (
    order_id UUID NOT NULL REFERENCES swap_orders(id),
    callback_id TEXT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (order_id, callback_id)
);

CREATE TABLE IF NOT EXISTS swap_receipts (
    receipt_id UUID PRIMARY KEY,
    order_id UUID NOT NULL UNIQUE REFERENCES swap_orders(id), -- generated exactly once
    tenant_id UUID NOT NULL,
    direction TEXT NOT NULL,
    status TEXT NOT NULL,
    source_asset TEXT NOT NULL,
    dest_asset TEXT NOT NULL,
    executed_amount TEXT NOT NULL,
    fee TEXT NOT NULL,
    refunded_amount TEXT NOT NULL DEFAULT '0',
    routing_path TEXT NOT NULL,
    price_reference TEXT NOT NULL,
    settlement_tx_id TEXT NOT NULL,
    settled_at TIMESTAMPTZ NOT NULL,
    content_hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_swap_receipts_tenant_settled ON swap_receipts (tenant_id, settled_at);

CREATE TABLE IF NOT EXISTS swap_console_audit_log (
    seq BIGSERIAL PRIMARY KEY,
    operator TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    reason TEXT NOT NULL,
    outcome TEXT NOT NULL,
    prev_hash TEXT NOT NULL,
    hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Receipts and the audit log are append-only.
CREATE OR REPLACE FUNCTION swap_reject_mutation() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION '% is immutable', TG_TABLE_NAME;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER swap_receipts_immutable BEFORE UPDATE OR DELETE ON swap_receipts
    FOR EACH ROW EXECUTE FUNCTION swap_reject_mutation();
CREATE TRIGGER swap_console_audit_log_immutable BEFORE UPDATE OR DELETE ON swap_console_audit_log
    FOR EACH ROW EXECUTE FUNCTION swap_reject_mutation();
