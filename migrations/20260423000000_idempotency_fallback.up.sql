-- Add database-backed idempotency fallback table
CREATE TABLE idempotency_keys (
    key VARCHAR(255) PRIMARY KEY,
    status VARCHAR(20) NOT NULL,
    response JSONB,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- Index for cleanup
CREATE INDEX idx_idempotency_keys_expires_at ON idempotency_keys(expires_at);

-- Index for lookups
CREATE INDEX idx_idempotency_keys_key ON idempotency_keys(key);