-- Add depends_on column to feature_flags table
ALTER TABLE feature_flags
ADD COLUMN depends_on TEXT[] DEFAULT '{}';

-- Create index for dependency lookups
CREATE INDEX idx_feature_flags_depends_on ON feature_flags USING GIN(depends_on);
