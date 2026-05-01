-- Extend settlements with dispute workflow fields
ALTER TABLE settlements
    ADD COLUMN IF NOT EXISTS dispute_reason TEXT,
    ADD COLUMN IF NOT EXISTS original_total_amount NUMERIC,
    ADD COLUMN IF NOT EXISTS reviewed_by TEXT,
    ADD COLUMN IF NOT EXISTS reviewed_at TIMESTAMPTZ;

-- Valid statuses: completed, pending_review, disputed, adjusted, voided
-- The status column already exists (VARCHAR 20); extend the check if present
CREATE OR REPLACE FUNCTION _add_settlements_status_check()
RETURNS void LANGUAGE plpgsql AS $func$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'settlements_status_check'
    ) THEN
        ALTER TABLE settlements ADD CONSTRAINT settlements_status_check
            CHECK (status IN ('completed','pending_review','disputed','adjusted','voided'));
    END IF;
END;
$func$;

SELECT _add_settlements_status_check();

DROP FUNCTION _add_settlements_status_check();
