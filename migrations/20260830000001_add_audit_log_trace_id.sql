-- Add trace_id to audit_logs so an audit entry emitted anywhere in the
-- webhook-to-reconciliation pipeline can be correlated back to the
-- transactions.trace_id it originated from (issue: propagate distributed
-- trace IDs across the full pipeline).
ALTER TABLE audit_logs ADD COLUMN IF NOT EXISTS trace_id VARCHAR(64);

CREATE INDEX IF NOT EXISTS idx_audit_logs_trace_id
    ON audit_logs (trace_id)
    WHERE trace_id IS NOT NULL;
