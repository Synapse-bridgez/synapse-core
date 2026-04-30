-- Migration: Create tenants table for multi-tenant support
-- Each tenant represents a Stellar Anchor Platform integration

CREATE TABLE IF NOT EXISTS tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    api_key VARCHAR(255) NOT NULL UNIQUE,
    webhook_secret VARCHAR(255) NOT NULL DEFAULT '',
    stellar_account VARCHAR(56) NOT NULL DEFAULT '',
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create index for faster lookups by API key (used in every request)
CREATE INDEX IF NOT EXISTS idx_tenants_api_key ON tenants(api_key);
