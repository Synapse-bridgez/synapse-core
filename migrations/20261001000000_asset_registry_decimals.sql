-- Asset registry: per-asset precision.
--
-- The assets table already carries identity (asset_code, asset_issuer), the
-- enable flag, min/max order amounts and display metadata. This adds the
-- missing precision dimension. Stellar classic assets use 7 decimal places,
-- so that is the default; the CHECK keeps values within what NUMERIC-backed
-- amounts can sensibly represent.
ALTER TABLE assets
    ADD COLUMN IF NOT EXISTS decimals INTEGER NOT NULL DEFAULT 7
        CHECK (decimals BETWEEN 0 AND 18);
