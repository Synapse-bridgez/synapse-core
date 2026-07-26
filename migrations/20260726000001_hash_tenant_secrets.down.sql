ALTER TABLE tenants ALTER COLUMN webhook_secret TYPE VARCHAR(255) USING convert_from(webhook_secret, 'UTF8');
ALTER TABLE tenants ALTER COLUMN webhook_secret SET DEFAULT '';

ALTER TABLE tenants RENAME COLUMN api_key_hash TO api_key;
