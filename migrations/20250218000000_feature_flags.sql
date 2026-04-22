-- Create feature_flags table
CREATE TABLE IF NOT EXISTS feature_flags (
    name VARCHAR(100) PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT false,
    description TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create index for faster queries
CREATE INDEX idx_feature_flags_enabled ON feature_flags(enabled);

-- Insert default feature flags
INSERT INTO feature_flags (name, enabled, description) VALUES
    ('experimental_processor', false, 'Enable experimental transaction processor logic'),
    ('new_asset_support', false, 'Enable support for new asset types')
ON CONFLICT (name) DO NOTHING;
