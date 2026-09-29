-- Create table for tracking tenant deletion requests and approvals
CREATE TABLE IF NOT EXISTS tenant_deletion_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    request_status VARCHAR(50) NOT NULL DEFAULT 'requested', -- requested, approved, executing, completed, rejected
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    requested_by_admin UUID NOT NULL, -- admin who requested deletion
    approved_by_admin UUID, -- admin who approved deletion (different from requester)
    approval_timestamp TIMESTAMP WITH TIME ZONE,
    execution_started_at TIMESTAMP WITH TIME ZONE,
    execution_completed_at TIMESTAMP WITH TIME ZONE,
    deletion_reason TEXT, -- why the tenant is being deleted
    rows_deleted_transactions BIGINT DEFAULT 0,
    rows_deleted_settlements BIGINT DEFAULT 0,
    rows_deleted_webhook_events BIGINT DEFAULT 0,
    rows_retained_audit_logs BIGINT DEFAULT 0, -- audit logs kept per compliance
    error_message TEXT,
    retention_floor_violations TEXT -- comma-separated list of records that couldn't be deleted due to retention
);

-- Index for querying deletion requests by tenant and status
CREATE INDEX IF NOT EXISTS idx_tenant_deletion_requests_tenant_status ON tenant_deletion_requests(tenant_id, request_status);
CREATE INDEX IF NOT EXISTS idx_tenant_deletion_requests_created ON tenant_deletion_requests(created_at);

-- Add RLS policy
ALTER TABLE tenant_deletion_requests ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_deletion_isolation ON tenant_deletion_requests
    USING (tenant_id = current_setting('app.current_tenant_id')::uuid OR current_setting('app.is_admin')::boolean = true);

-- Create table to track in-flight transactions/disputes that block deletion
CREATE TABLE IF NOT EXISTS tenant_deletion_blockers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    deletion_request_id UUID NOT NULL REFERENCES tenant_deletion_requests(id) ON DELETE CASCADE,
    blocker_type VARCHAR(50) NOT NULL, -- 'open_transaction', 'pending_settlement', 'disputed_payment'
    blocker_record_id UUID NOT NULL,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMP WITH TIME ZONE
);

CREATE INDEX IF NOT EXISTS idx_deletion_blockers_request ON tenant_deletion_blockers(deletion_request_id);
CREATE INDEX IF NOT EXISTS idx_deletion_blockers_resolved ON tenant_deletion_blockers(resolved_at);

ALTER TABLE tenant_deletion_blockers ENABLE ROW LEVEL SECURITY;
CREATE POLICY deletion_blockers_isolation ON tenant_deletion_blockers
    USING (tenant_id = current_setting('app.current_tenant_id')::uuid OR current_setting('app.is_admin')::boolean = true);
