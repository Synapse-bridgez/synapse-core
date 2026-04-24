ALTER TABLE transactions
    DROP COLUMN IF EXISTS metadata,
    DROP COLUMN IF EXISTS memo_type,
    DROP COLUMN IF EXISTS memo;
