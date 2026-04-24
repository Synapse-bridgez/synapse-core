-- Add max_delivery_rate column to webhook_endpoints table
-- Default rate limit is 10 deliveries per minute

ALTER TABLE webhook_endpoints
ADD COLUMN max_delivery_rate INTEGER NOT NULL DEFAULT 10;