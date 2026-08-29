ALTER TABLE tenants DROP COLUMN IF EXISTS previous_api_key_hash;
ALTER TABLE tenants DROP COLUMN IF EXISTS grace_period_expires_at;
