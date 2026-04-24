-- Remove unique constraint from webhook_deliveries table

ALTER TABLE webhook_deliveries
DROP CONSTRAINT IF EXISTS unique_webhook_delivery;