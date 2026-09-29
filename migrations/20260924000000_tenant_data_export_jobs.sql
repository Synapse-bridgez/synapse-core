-- Create table for tracking tenant data export jobs
CREATE TABLE IF NOT EXISTS tenant_data_export_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    job_status VARCHAR(50) NOT NULL DEFAULT 'pending', -- pending, in_progress, completed, failed
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    started_at TIMESTAMP WITH TIME ZONE,
    completed_at TIMESTAMP WITH TIME ZONE,
    requested_by_admin UUID NOT NULL, -- admin user ID who requested the export
    archive_location VARCHAR(500), -- S3 or local path where the archive is stored
    archive_size_bytes BIGINT, -- size of the generated archive
    error_message TEXT, -- if status is failed
    export_scope VARCHAR(50) NOT NULL DEFAULT 'full', -- full, transactions, settlements
    row_count_transactions BIGINT DEFAULT 0,
    row_count_settlements BIGINT DEFAULT 0,
    row_count_audit_logs BIGINT DEFAULT 0,
    row_count_webhook_events BIGINT DEFAULT 0,
    retention_days INT DEFAULT 30, -- how long to keep the archive
    delete_after TIMESTAMP WITH TIME ZONE -- when the archive expires
);

-- Index for querying jobs by tenant and status
CREATE INDEX IF NOT EXISTS idx_tenant_export_jobs_tenant_status ON tenant_data_export_jobs(tenant_id, job_status);
CREATE INDEX IF NOT EXISTS idx_tenant_export_jobs_created ON tenant_data_export_jobs(created_at);

-- Add RLS policy if RLS is enabled on tenants table
ALTER TABLE tenant_data_export_jobs ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_export_isolation ON tenant_data_export_jobs
    USING (tenant_id = current_setting('app.current_tenant_id')::uuid OR current_setting('app.is_admin')::boolean = true);
