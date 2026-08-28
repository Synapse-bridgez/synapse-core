-- Add a per-asset hold period to the asset processing rules.
--
-- min_amount / max_amount / settlement_schedule already exist from
-- 20260428000001_asset_processing_rules.sql; the enable/disable flag is the
-- pre-existing assets.enabled column. This adds the last missing rule
-- dimension: how long a newly ingested transaction for the asset must be
-- held before the processor is allowed to complete it.
--
-- Safe for blue-green: NOT NULL with a DEFAULT, on a low-traffic config
-- table (not the partitioned transactions table). Old app ignores the
-- column; new app reads it. The >= 0 invariant is enforced in the admin
-- write path (handlers/admin/mod.rs), not as a DB constraint, to keep this
-- migration a single non-locking column add.
ALTER TABLE assets
    ADD COLUMN IF NOT EXISTS hold_period_seconds BIGINT NOT NULL DEFAULT 0;
