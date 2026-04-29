-- Create feature_flag_audit_logs table
CREATE TABLE IF NOT EXISTS feature_flag_audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    flag_name VARCHAR(100) NOT NULL,
    old_value JSONB,
    new_value JSONB,
    actor VARCHAR(255) NOT NULL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create indexes for efficient queries
CREATE INDEX idx_feature_flag_audit_flag_name ON feature_flag_audit_logs(flag_name);
CREATE INDEX idx_feature_flag_audit_timestamp ON feature_flag_audit_logs(timestamp DESC);
CREATE INDEX idx_feature_flag_audit_actor ON feature_flag_audit_logs(actor);
CREATE INDEX idx_feature_flag_audit_flag_timestamp ON feature_flag_audit_logs(flag_name, timestamp DESC);
