-- Revert to the previous partition function implementation
-- This restores the hardcoded lookahead behavior

CREATE OR REPLACE FUNCTION create_monthly_partition()
RETURNS void AS $$
BEGIN
    PERFORM ensure_partition_for((NOW() + INTERVAL '2 months')::DATE);
END;
$$ LANGUAGE plpgsql;

-- Drop the configurable function (it will be recreated on the next migration forward)
DROP FUNCTION IF EXISTS ensure_future_partitions(INT);
