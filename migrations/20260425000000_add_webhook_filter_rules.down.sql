-- Remove filter_rules column from webhook_endpoints table

ALTER TABLE webhook_endpoints
DROP COLUMN IF EXISTS filter_rules;