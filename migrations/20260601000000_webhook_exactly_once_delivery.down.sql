DROP TABLE IF EXISTS webhook_delivery_dlq;

ALTER TABLE webhook_endpoints
    DROP COLUMN IF EXISTS circuit_opened_at,
    DROP COLUMN IF EXISTS circuit_failure_count,
    DROP COLUMN IF EXISTS circuit_state;

ALTER TABLE webhook_deliveries
    DROP COLUMN IF EXISTS claimed_at,
    DROP COLUMN IF EXISTS attempt_history;
