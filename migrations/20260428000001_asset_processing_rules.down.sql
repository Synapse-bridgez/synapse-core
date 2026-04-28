ALTER TABLE assets
    DROP COLUMN IF EXISTS min_amount,
    DROP COLUMN IF EXISTS max_amount,
    DROP COLUMN IF EXISTS settlement_schedule;
