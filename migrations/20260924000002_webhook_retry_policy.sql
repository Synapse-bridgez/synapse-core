-- Add per-endpoint configurable webhook retry policy (Issue #1258)
-- Each webhook endpoint can define its own retry behavior with sensible defaults

ALTER TABLE webhook_endpoints
ADD COLUMN IF NOT EXISTS retry_policy JSONB NOT NULL DEFAULT jsonb_build_object(
    'max_attempts', 5,
    'base_delay_secs', 10,
    'multiplier', 2.0,
    'max_delay_secs', 300
);

-- Constraint: Ensure retry_policy has required fields and sensible bounds
ALTER TABLE webhook_endpoints
ADD CONSTRAINT check_retry_policy_valid CHECK (
    retry_policy ? 'max_attempts'
    AND retry_policy ? 'base_delay_secs'
    AND retry_policy ? 'multiplier'
    AND retry_policy ? 'max_delay_secs'
    AND (retry_policy->>'max_attempts')::int >= 1
    AND (retry_policy->>'max_attempts')::int <= 50
    AND (retry_policy->>'base_delay_secs')::int >= 1
    AND (retry_policy->>'base_delay_secs')::int <= 3600
    AND (retry_policy->>'multiplier')::float > 1.0
    AND (retry_policy->>'multiplier')::float <= 10.0
    AND (retry_policy->>'max_delay_secs')::int >= 60
    AND (retry_policy->>'max_delay_secs')::int <= 86400
);

-- Index for efficient policy lookups
CREATE INDEX IF NOT EXISTS idx_webhook_endpoints_retry_policy
ON webhook_endpoints USING GIN (retry_policy);
