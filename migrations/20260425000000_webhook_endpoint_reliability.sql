-- Add reliability tracking columns to webhook_endpoints (if the table exists).
-- If the table does not yet exist it is created here so the migration is
-- idempotent regardless of the order other migrations run.

CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    url           TEXT        NOT NULL,
    secret        TEXT,
    enabled       BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Reliability tracking columns
ALTER TABLE webhook_endpoints
    ADD COLUMN IF NOT EXISTS success_rate      NUMERIC(5, 2) NOT NULL DEFAULT 100.00,
    ADD COLUMN IF NOT EXISTS total_deliveries  INT           NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS last_success_at   TIMESTAMPTZ;

-- Per-delivery event log used to compute the rolling 24-hour window
CREATE TABLE IF NOT EXISTS webhook_delivery_events (
    id                  UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint_id         UUID        NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    delivered_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    success             BOOLEAN     NOT NULL,
    http_status         INT,
    response_time_ms    INT,
    error_message       TEXT
);

CREATE INDEX IF NOT EXISTS idx_wde_endpoint_delivered
    ON webhook_delivery_events (endpoint_id, delivered_at DESC);

CREATE INDEX IF NOT EXISTS idx_wde_delivered_at
    ON webhook_delivery_events (delivered_at DESC);

-- Notification log for auto-disable events
CREATE TABLE IF NOT EXISTS webhook_endpoint_notifications (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    endpoint_id     UUID        NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    reason          TEXT        NOT NULL,
    success_rate    NUMERIC(5, 2),
    notified_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_wen_endpoint_id
    ON webhook_endpoint_notifications (endpoint_id, notified_at DESC);
