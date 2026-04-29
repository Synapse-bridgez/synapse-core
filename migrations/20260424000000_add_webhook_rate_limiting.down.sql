-- Remove max_delivery_rate column from webhook_endpoints table

ALTER TABLE webhook_endpoints
DROP COLUMN IF EXISTS max_delivery_rate;