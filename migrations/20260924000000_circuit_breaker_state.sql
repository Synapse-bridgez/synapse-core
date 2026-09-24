-- Add circuit breaker state table for multi-region coordination
CREATE TABLE IF NOT EXISTS circuit_breaker_state (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_name TEXT NOT NULL,
    region_name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('closed', 'open', 'half_open')),
    failure_count INTEGER NOT NULL DEFAULT 0,
    opened_at TIMESTAMPTZ,
    last_error TEXT,
    synced BOOLEAN NOT NULL DEFAULT false,
    last_sync_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(service_name, region_name)
);

-- Create index for efficient queries by service name
CREATE INDEX IF NOT EXISTS idx_circuit_breaker_service
ON circuit_breaker_state(service_name);

-- Create index for efficient queries by region
CREATE INDEX IF NOT EXISTS idx_circuit_breaker_region
ON circuit_breaker_state(region_name);

-- Create index for finding stale entries
CREATE INDEX IF NOT EXISTS idx_circuit_breaker_last_sync
ON circuit_breaker_state(last_sync_at);

-- Add comment documenting the table purpose
COMMENT ON TABLE circuit_breaker_state IS
'Tracks circuit breaker state across regions for multi-region deployment.
Serves as the central source of truth for circuit state coordination.';
