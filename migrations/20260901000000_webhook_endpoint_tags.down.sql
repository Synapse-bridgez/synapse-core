-- Rollback: Remove tags support from webhook_endpoints table
DROP INDEX IF EXISTS idx_webhook_endpoints_tags_gin;

ALTER TABLE webhook_endpoints
DROP CONSTRAINT IF EXISTS max_tags_per_endpoint;

ALTER TABLE webhook_endpoints
DROP CONSTRAINT IF EXISTS valid_tag_names;

ALTER TABLE webhook_endpoints
DROP COLUMN IF EXISTS tags;
