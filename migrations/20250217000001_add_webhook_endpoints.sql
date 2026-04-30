-- Create webhook_endpoints table
CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    url VARCHAR(500) NOT NULL,
    secret VARCHAR(255),  -- optional secret for webhook verification
    circuit_state VARCHAR(20) NOT NULL DEFAULT 'closed',
    circuit_failure_count INTEGER NOT NULL DEFAULT 0,
    circuit_opened_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create index
CREATE INDEX idx_webhook_endpoints_url ON webhook_endpoints(url);