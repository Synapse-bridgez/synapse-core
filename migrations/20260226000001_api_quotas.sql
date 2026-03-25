-- API quota configurations
CREATE TABLE IF NOT EXISTS api_quotas (
    key VARCHAR(255) PRIMARY KEY,
    tier VARCHAR(20) NOT NULL DEFAULT 'free',
    custom_limit INTEGER,
    reset_schedule VARCHAR(20) NOT NULL DEFAULT 'hourly',
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_api_quotas_tier ON api_quotas(tier);
CREATE INDEX idx_api_quotas_updated ON api_quotas(updated_at);
