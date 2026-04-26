-- Add filter_rules JSONB column to webhook_endpoints table
-- Filter rules allow endpoints to filter events by transaction properties
-- Example: {"asset_codes": ["USD", "EUR"], "min_amount": "100.00"}

ALTER TABLE webhook_endpoints
ADD COLUMN filter_rules JSONB;