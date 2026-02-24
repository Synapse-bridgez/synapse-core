-- Create transactions table with tenant_id for isolation
CREATE TABLE IF NOT EXISTS transactions (
    transaction_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    external_id VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    amount VARCHAR(50) NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    stellar_transaction_id VARCHAR(64),
    memo TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Ensure external_id is unique per tenant
    CONSTRAINT unique_external_id_per_tenant UNIQUE (tenant_id, external_id)
);

-- Create index on tenant_id for row-level filtering
CREATE INDEX idx_transactions_tenant_id ON transactions(tenant_id);

-- Create composite index for common queries
CREATE INDEX idx_transactions_tenant_status ON transactions(tenant_id, status);
CREATE INDEX idx_transactions_tenant_created ON transactions(tenant_id, created_at DESC);

-- Create index on stellar_transaction_id for lookups
CREATE INDEX idx_transactions_stellar_id ON transactions(stellar_transaction_id) WHERE stellar_transaction_id IS NOT NULL;

-- Add Row Level Security (RLS) policies
ALTER TABLE transactions ENABLE ROW LEVEL SECURITY;

-- Policy: Users can only see transactions for their tenant
-- Note: This requires setting the tenant_id in the session context
-- For application-level enforcement, we rely on WHERE tenant_id = $1 in queries

-- Add comments
COMMENT ON TABLE transactions IS 'Stores transaction data with tenant isolation';
COMMENT ON COLUMN transactions.tenant_id IS 'Foreign key to tenants table - enforces data isolation';
COMMENT ON COLUMN transactions.external_id IS 'External identifier from the anchor/partner system';
COMMENT ON COLUMN transactions.status IS 'Transaction status: pending, processing, completed, failed';
COMMENT ON COLUMN transactions.stellar_transaction_id IS 'Stellar blockchain transaction hash';
