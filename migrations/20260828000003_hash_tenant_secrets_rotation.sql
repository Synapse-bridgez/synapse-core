-- Migration: Add secret rotation support to tenants table

ALTER TABLE tenants
ADD COLUMN IF NOT EXISTS previous_webhook_secret VARCHAR(255),
ADD COLUMN IF NOT EXISTS previous_secret_expires_at TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS secret_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

CREATE INDEX IF NOT EXISTS idx_tenants_previous_secret_expires ON tenants(previous_secret_expires_at)
WHERE previous_secret_expires_at IS NOT NULL;
