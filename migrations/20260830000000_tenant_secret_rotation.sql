-- Migration: Add secret rotation support for tenants
-- Allows dual-validation of old and new API keys during a configurable grace period.

ALTER TABLE tenants
    ADD COLUMN IF NOT EXISTS previous_api_key_hash VARCHAR(255),
    ADD COLUMN IF NOT EXISTS grace_period_expires_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS idx_tenants_previous_api_key_hash
    ON tenants (previous_api_key_hash)
    WHERE previous_api_key_hash IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_tenants_grace_period_expires_at
    ON tenants (grace_period_expires_at)
    WHERE grace_period_expires_at IS NOT NULL;
