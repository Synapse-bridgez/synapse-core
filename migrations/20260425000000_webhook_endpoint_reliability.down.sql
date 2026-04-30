DROP TABLE IF EXISTS webhook_endpoint_notifications CASCADE;
DROP TABLE IF EXISTS webhook_delivery_events CASCADE;

ALTER TABLE webhook_endpoints
    DROP COLUMN IF EXISTS success_rate,
    DROP COLUMN IF EXISTS total_deliveries,
    DROP COLUMN IF EXISTS last_success_at;
