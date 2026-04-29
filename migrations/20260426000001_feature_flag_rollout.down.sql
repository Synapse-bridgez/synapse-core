-- Rollback feature flag rollout percentage
DROP INDEX IF EXISTS idx_feature_flags_rollout;
ALTER TABLE feature_flags DROP COLUMN IF EXISTS rollout_percentage;
