ALTER TABLE settlements DROP CONSTRAINT IF EXISTS settlements_status_check;
ALTER TABLE settlements
    DROP COLUMN IF EXISTS dispute_reason,
    DROP COLUMN IF EXISTS original_total_amount,
    DROP COLUMN IF EXISTS reviewed_by,
    DROP COLUMN IF EXISTS reviewed_at;
