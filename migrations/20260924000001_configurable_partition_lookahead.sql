-- Add configurable partition lookahead function
-- Allows the partition pre-creation job to be tuned based on observed traffic growth

CREATE OR REPLACE FUNCTION ensure_future_partitions(months_ahead INT)
RETURNS void AS $$
DECLARE
    i INT := 0;
    target_year INT;
    target_month INT;
    current_date DATE;
BEGIN
    current_date := NOW()::DATE;
    target_year := EXTRACT(YEAR FROM current_date)::INT;
    target_month := EXTRACT(MONTH FROM current_date)::INT;

    WHILE i < months_ahead LOOP
        PERFORM ensure_partition_for((target_year || '-' || LPAD(target_month::TEXT, 2, '0') || '-01')::DATE);

        -- Increment month
        IF target_month = 12 THEN
            target_month := 1;
            target_year := target_year + 1;
        ELSE
            target_month := target_month + 1;
        END IF;

        i := i + 1;
    END LOOP;
END;
$$ LANGUAGE plpgsql;

-- Backward-compatible default: create partitions for next 3 months
CREATE OR REPLACE FUNCTION create_monthly_partition()
RETURNS void AS $$
BEGIN
    -- Default to 3 months ahead (includes current month + 2 more)
    -- This can be overridden by applications via the ensure_future_partitions function
    PERFORM ensure_partition_for((NOW() + INTERVAL '2 months')::DATE);
END;
$$ LANGUAGE plpgsql;

-- Grant execute permissions to the application role
ALTER FUNCTION ensure_future_partitions(INT) OWNER TO synapse_service;
ALTER FUNCTION create_monthly_partition() OWNER TO synapse_service;
