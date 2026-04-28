-- Add asset-level processing configuration columns
ALTER TABLE assets
    ADD COLUMN IF NOT EXISTS min_amount NUMERIC,
    ADD COLUMN IF NOT EXISTS max_amount NUMERIC,
    ADD COLUMN IF NOT EXISTS settlement_schedule VARCHAR(50) DEFAULT 'daily';

-- Update existing seed assets with sensible defaults
UPDATE assets SET
    min_amount = 1.00,
    max_amount = 1000000.00,
    settlement_schedule = 'daily'
WHERE min_amount IS NULL;
