-- Rollback Migration: Remove secret rotation support from tenants table

DROP INDEX IF EXISTS idx_tenants_previous_secret_expires;

ALTER TABLE tenants
DROP COLUMN IF EXISTS previous_webhook_secret,
DROP COLUMN IF EXISTS previous_secret_expires_at,
DROP COLUMN IF EXISTS secret_updated_at;
