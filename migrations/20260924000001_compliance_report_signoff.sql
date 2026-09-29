-- Add compliance report status and reviewer sign-off fields
ALTER TABLE compliance_reports ADD COLUMN IF NOT EXISTS status VARCHAR(20) NOT NULL DEFAULT 'pending_review'
CHECK (status IN ('pending_review', 'approved', 'rejected'));

ALTER TABLE compliance_reports ADD COLUMN IF NOT EXISTS reviewed_by UUID;

ALTER TABLE compliance_reports ADD COLUMN IF NOT EXISTS reviewed_at TIMESTAMPTZ;

ALTER TABLE compliance_reports ADD COLUMN IF NOT EXISTS reviewer_notes TEXT;

-- Add updated_at column to track when status changes occur
ALTER TABLE compliance_reports ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Create index for finding reports by status
CREATE INDEX IF NOT EXISTS idx_compliance_reports_status
ON compliance_reports(status);

-- Create index for finding reports by reviewer
CREATE INDEX IF NOT EXISTS idx_compliance_reports_reviewed_by
ON compliance_reports(reviewed_by);

-- Add comment documenting the new columns
COMMENT ON COLUMN compliance_reports.status IS
'Report status: pending_review (awaiting reviewer approval), approved (signed off by reviewer), rejected (reviewer rejected the report)';

COMMENT ON COLUMN compliance_reports.reviewed_by IS
'UUID of the user who reviewed/signed off on this report';

COMMENT ON COLUMN compliance_reports.reviewed_at IS
'Timestamp when the report was reviewed and signed off';

COMMENT ON COLUMN compliance_reports.reviewer_notes IS
'Optional notes from the reviewer explaining approval or rejection';
