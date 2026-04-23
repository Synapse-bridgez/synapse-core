DROP INDEX IF EXISTS idx_webhook_deliveries_next_attempt;
DROP INDEX IF EXISTS idx_webhook_deliveries_status;
DROP INDEX IF EXISTS idx_webhook_deliveries_transaction_id;
DROP INDEX IF EXISTS idx_webhook_deliveries_endpoint_id;
DROP TABLE IF EXISTS webhook_deliveries;
DROP INDEX IF EXISTS idx_webhook_endpoints_enabled;
DROP TABLE IF EXISTS webhook_endpoints;
