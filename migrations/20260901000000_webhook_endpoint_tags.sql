-- Add tags array column to webhook_endpoints table for bulk operations
ALTER TABLE webhook_endpoints
ADD COLUMN tags TEXT[] DEFAULT '{}'::TEXT[];

-- Create GIN index for efficient tag-based filtering
CREATE INDEX idx_webhook_endpoints_tags_gin ON webhook_endpoints USING GIN (tags);

-- Add constraint to limit tags per endpoint to 100 (prevent abuse)
ALTER TABLE webhook_endpoints
ADD CONSTRAINT max_tags_per_endpoint CHECK (array_length(tags, 1) <= 100);

-- Add constraint to validate tag names (alphanumeric, hyphens, underscores, max 50 chars per tag)
ALTER TABLE webhook_endpoints
ADD CONSTRAINT valid_tag_names CHECK (
    NOT EXISTS (
        SELECT 1 FROM (
            SELECT unnest(tags) as tag
        ) t
        WHERE tag ~ '[^a-zA-Z0-9\-_]' OR length(tag) > 50
    )
);

-- Add updated_at trigger to refresh timestamp when tags change
-- (assumes trigger already exists from initial schema, this just documents the behavior)
