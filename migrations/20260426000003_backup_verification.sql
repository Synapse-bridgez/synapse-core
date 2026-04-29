-- Create backup_verification_logs table
CREATE TABLE IF NOT EXISTS backup_verification_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    backup_filename VARCHAR(255) NOT NULL,
    verification_status VARCHAR(50) NOT NULL,
    row_count BIGINT,
    latest_timestamp TIMESTAMPTZ,
    error_message TEXT,
    verified_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Create indexes for efficient queries
CREATE INDEX idx_backup_verification_filename ON backup_verification_logs(backup_filename);
CREATE INDEX idx_backup_verification_status ON backup_verification_logs(verification_status);
CREATE INDEX idx_backup_verification_verified_at ON backup_verification_logs(verified_at DESC);
