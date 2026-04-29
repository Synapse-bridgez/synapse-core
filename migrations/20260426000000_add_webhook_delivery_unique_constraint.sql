-- Add unique constraint to prevent duplicate webhook deliveries
-- Ensures only one delivery per endpoint/transaction/event_type combination

ALTER TABLE webhook_deliveries
ADD CONSTRAINT unique_webhook_delivery
UNIQUE (endpoint_id, transaction_id, event_type);