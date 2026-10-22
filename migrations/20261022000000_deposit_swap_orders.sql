-- Keep the partition key in all source references; tenant is part of the FK
-- so even direct SQL cannot attach another tenant's deposit to an order.
ALTER TABLE transactions ADD CONSTRAINT transactions_source_tenant_key
    UNIQUE (id, created_at, tenant_id);

CREATE TABLE deposit_callback_receipts (
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id),
    anchor_transaction_id TEXT NOT NULL,
    callback_type TEXT NOT NULL,
    transaction_id UUID NOT NULL,
    transaction_created_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (tenant_id, anchor_transaction_id, callback_type),
    FOREIGN KEY (transaction_id, transaction_created_at, tenant_id)
        REFERENCES transactions(id, created_at, tenant_id)
);

CREATE TABLE swap_orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id),
    source_transaction_id UUID NOT NULL UNIQUE,
    source_transaction_created_at TIMESTAMPTZ NOT NULL,
    asset_code TEXT NOT NULL,
    amount NUMERIC NOT NULL CHECK (amount > 0),
    status TEXT NOT NULL DEFAULT 'swap_ready' CHECK (status = 'swap_ready'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (source_transaction_id, source_transaction_created_at, tenant_id)
        REFERENCES transactions(id, created_at, tenant_id)
);
CREATE INDEX swap_orders_tenant_created ON swap_orders(tenant_id, created_at);

ALTER TABLE deposit_callback_receipts ENABLE ROW LEVEL SECURITY;
ALTER TABLE deposit_callback_receipts FORCE ROW LEVEL SECURITY;
CREATE POLICY deposit_callback_tenant ON deposit_callback_receipts
    USING (tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true')
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true');
ALTER TABLE swap_orders ENABLE ROW LEVEL SECURITY;
ALTER TABLE swap_orders FORCE ROW LEVEL SECURITY;
CREATE POLICY swap_order_tenant ON swap_orders
    USING (tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true')
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true)
        OR current_setting('app.is_admin', true) = 'true');

INSERT INTO feature_flags(name, enabled, description, rollout_percentage)
VALUES ('swap_engine', false, 'Automatically prepare swap orders for qualifying deposits', 0)
ON CONFLICT (name) DO NOTHING;
