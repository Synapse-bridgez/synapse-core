-- Add rollout_percentage column to feature_flags table
ALTER TABLE feature_flags ADD COLUMN rollout_percentage INT DEFAULT 100 CHECK (rollout_percentage >= 0 AND rollout_percentage <= 100);

-- Create index for rollout queries
CREATE INDEX idx_feature_flags_rollout ON feature_flags(rollout_percentage);
