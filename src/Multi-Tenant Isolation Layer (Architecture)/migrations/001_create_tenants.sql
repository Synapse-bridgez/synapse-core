-- Create tenants table
CREATE TABLE IF NOT EXISTS tenants (
    tenant_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL,
    api_key VARCHAR(255) NOT NULL UNIQUE,
    webhook_secret VARCHAR(255) NOT NULL,
    stellar_account VARCHAR(56) NOT NULL,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create index on api_key for fast lookups
CREATE INDEX idx_tenants_api_key ON tenants(api_key) WHERE is_active = true;

-- Create index on is_active
CREATE INDEX idx_tenants_active ON tenants(is_active);

-- Add comment
COMMENT ON TABLE tenants IS 'Stores tenant configurations for multi-tenant isolation';
COMMENT ON COLUMN tenants.api_key IS 'API key used to identify tenant in requests';
COMMENT ON COLUMN tenants.webhook_secret IS 'Secret for validating webhook signatures';
COMMENT ON COLUMN tenants.stellar_account IS 'Stellar account address for this tenant';
COMMENT ON COLUMN tenants.rate_limit_per_minute IS 'Rate limit for API requests per minute';
