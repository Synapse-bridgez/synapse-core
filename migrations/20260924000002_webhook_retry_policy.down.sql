-- Drop retry_policy column and its constraint
ALTER TABLE webhook_endpoints
DROP CONSTRAINT IF EXISTS check_retry_policy_valid;

ALTER TABLE webhook_endpoints
DROP COLUMN IF EXISTS retry_policy;
