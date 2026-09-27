DROP INDEX IF EXISTS idx_audit_logs_trace_id;
ALTER TABLE audit_logs DROP COLUMN IF EXISTS trace_id;
