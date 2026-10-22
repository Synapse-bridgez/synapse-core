-- #1535: Per-tenant swap notification preferences and versioned templates.
-- Rendering logic lives in src/swap/notifications.rs.

CREATE TABLE IF NOT EXISTS swap_notification_preferences (
    tenant_id          UUID        PRIMARY KEY,
    subscribed_events  TEXT[]      NOT NULL DEFAULT '{}',   -- empty = all events
    channels           TEXT[]      NOT NULL DEFAULT '{webhook}',
    delivery_mode      TEXT        NOT NULL DEFAULT 'immediate' CHECK (delivery_mode IN ('immediate', 'digest')),
    emails             TEXT[]      NOT NULL DEFAULT '{}',
    webhook_urls       TEXT[]      NOT NULL DEFAULT '{}',
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (cardinality(emails) + cardinality(webhook_urls) <= 20)
);

CREATE TABLE IF NOT EXISTS swap_notification_templates (
    tenant_id   UUID        NOT NULL,
    event_type  TEXT        NOT NULL,
    version     INTEGER     NOT NULL,
    body        TEXT        NOT NULL CHECK (octet_length(body) <= 4096),
    active      BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, event_type, version)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_swap_notification_templates_active
    ON swap_notification_templates (tenant_id, event_type) WHERE active;

-- Delivery log: render failures are recorded here, never dropped.
CREATE TABLE IF NOT EXISTS swap_notification_deliveries (
    id          BIGSERIAL   PRIMARY KEY,
    tenant_id   UUID        NOT NULL,
    event_type  TEXT        NOT NULL,
    channel     TEXT        NOT NULL,
    status      TEXT        NOT NULL CHECK (status IN ('ready', 'failed')),
    error       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_swap_notification_deliveries_tenant
    ON swap_notification_deliveries (tenant_id, created_at DESC);
