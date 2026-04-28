-- Rollback feature flag dependencies
DROP INDEX IF EXISTS idx_feature_flags_depends_on;
ALTER TABLE feature_flags DROP COLUMN depends_on;
