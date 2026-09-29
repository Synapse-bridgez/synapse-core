-- Add webhook endpoint redirect configuration for migrations (Issue #1259)
-- Enables time-bounded redirects to reroute webhook traffic during infrastructure migrations

CREATE TABLE IF NOT EXISTS webhook_endpoint_redirects (
    id BIGSERIAL PRIMARY KEY,
    endpoint_id UUID NOT NULL REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    redirect_url VARCHAR(2048) NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    started_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
    expires_at TIMESTAMP WITH TIME ZONE NOT NULL,
    cancelled_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
    metadata JSONB DEFAULT NULL,
    FOREIGN KEY (endpoint_id) REFERENCES webhook_endpoints(id) ON DELETE CASCADE,
    INDEX idx_endpoint_expires (endpoint_id, expires_at),
    INDEX idx_active_redirects (endpoint_id, enabled) WHERE enabled = true AND cancelled_at IS NULL,
    CHECK (expires_at > created_at),
    UNIQUE (endpoint_id, redirect_url, cancelled_at) WHERE cancelled_at IS NULL
);

-- RLS policy: Tenants can only see redirects for their endpoints
ALTER TABLE webhook_endpoint_redirects ENABLE ROW LEVEL SECURITY;

CREATE POLICY webhook_redirects_tenant_isolation ON webhook_endpoint_redirects
    USING (
        endpoint_id IN (
            SELECT id FROM webhook_endpoints WHERE id IN (
                SELECT DISTINCT endpoint_id FROM webhook_deliveries
                WHERE transaction_id IN (
                    SELECT id FROM transactions WHERE tenant_id = current_setting('app.tenant_id')::UUID
                )
            )
        )
    );

-- Track redirect delivery metrics separately
CREATE TABLE IF NOT EXISTS webhook_redirect_deliveries (
    id BIGSERIAL PRIMARY KEY,
    redirect_id BIGINT NOT NULL REFERENCES webhook_endpoint_redirects(id) ON DELETE CASCADE,
    delivery_id UUID NOT NULL,
    attempt_count INT DEFAULT 0,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    last_attempt_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
    next_attempt_at TIMESTAMP WITH TIME ZONE DEFAULT NULL,
    response_status INT DEFAULT NULL,
    response_body TEXT DEFAULT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT CURRENT_TIMESTAMP,
    INDEX idx_redirect_status (redirect_id, status),
    INDEX idx_delivery_redirect (delivery_id, redirect_id),
    FOREIGN KEY (redirect_id) REFERENCES webhook_endpoint_redirects(id) ON DELETE CASCADE
);

-- Create index for efficient redirect lookups
CREATE INDEX IF NOT EXISTS idx_webhook_redirects_endpoint_active
    ON webhook_endpoint_redirects(endpoint_id)
    WHERE enabled = true AND cancelled_at IS NULL AND expires_at > CURRENT_TIMESTAMP;
