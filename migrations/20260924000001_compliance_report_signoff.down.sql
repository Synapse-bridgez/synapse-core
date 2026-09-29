-- Revert compliance report status and reviewer sign-off fields
DROP INDEX IF EXISTS idx_compliance_reports_reviewed_by;
DROP INDEX IF EXISTS idx_compliance_reports_status;

ALTER TABLE compliance_reports DROP COLUMN IF EXISTS reviewer_notes;
ALTER TABLE compliance_reports DROP COLUMN IF EXISTS reviewed_at;
ALTER TABLE compliance_reports DROP COLUMN IF EXISTS reviewed_by;
ALTER TABLE compliance_reports DROP COLUMN IF EXISTS status;
ALTER TABLE compliance_reports DROP COLUMN IF EXISTS updated_at;
