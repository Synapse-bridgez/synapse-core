-- Create materialized views for reporting aggregates to improve query performance

-- Daily transaction volume per tenant
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_daily_transaction_volume AS
SELECT
    DATE(created_at) as volume_date,
    tenant_id,
    asset_code,
    COUNT(*) as transaction_count,
    SUM(CAST(amount AS NUMERIC)) as total_volume,
    AVG(CAST(amount AS NUMERIC)) as avg_amount,
    MAX(CAST(amount AS NUMERIC)) as max_amount,
    MIN(CAST(amount AS NUMERIC)) as min_amount
FROM transactions
WHERE created_at >= NOW() - INTERVAL '90 days'
GROUP BY DATE(created_at), tenant_id, asset_code;

-- Create index for efficient querying
CREATE INDEX IF NOT EXISTS idx_mv_daily_tx_volume_date_tenant
    ON mv_daily_transaction_volume(volume_date DESC, tenant_id);

-- Daily settlement summary per tenant
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_daily_settlement_summary AS
SELECT
    DATE(created_at) as settlement_date,
    tenant_id,
    status,
    COUNT(*) as settlement_count,
    SUM(CAST(amount AS NUMERIC)) as total_amount,
    AVG(CAST(amount AS NUMERIC)) as avg_amount
FROM settlements
WHERE created_at >= NOW() - INTERVAL '90 days'
GROUP BY DATE(created_at), tenant_id, status;

-- Create index for efficient querying
CREATE INDEX IF NOT EXISTS idx_mv_daily_settlement_date_tenant
    ON mv_daily_settlement_summary(settlement_date DESC, tenant_id);

-- Transaction status distribution per tenant
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_transaction_status_distribution AS
SELECT
    tenant_id,
    status,
    COUNT(*) as count,
    DATE_TRUNC('hour', created_at) as hour_bucket,
    AVG(EXTRACT(EPOCH FROM (updated_at - created_at))) as avg_duration_seconds
FROM transactions
WHERE created_at >= NOW() - INTERVAL '30 days'
GROUP BY tenant_id, status, DATE_TRUNC('hour', created_at);

-- Create index for efficient querying
CREATE INDEX IF NOT EXISTS idx_mv_tx_status_dist_tenant_hour
    ON mv_transaction_status_distribution(tenant_id, hour_bucket DESC);

-- Hourly request volume by tenant
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_hourly_request_volume AS
SELECT
    DATE_TRUNC('hour', created_at) as hour_bucket,
    tenant_id,
    COUNT(*) as request_count
FROM transactions
WHERE created_at >= NOW() - INTERVAL '7 days'
GROUP BY DATE_TRUNC('hour', created_at), tenant_id;

-- Create index for efficient querying
CREATE INDEX IF NOT EXISTS idx_mv_hourly_requests_tenant
    ON mv_hourly_request_volume(hour_bucket DESC, tenant_id);

-- Asset performance summary (slowest/fastest settling)
CREATE MATERIALIZED VIEW IF NOT EXISTS mv_asset_performance_summary AS
SELECT
    tenant_id,
    asset_code,
    COUNT(*) as transaction_count,
    AVG(EXTRACT(EPOCH FROM (updated_at - created_at))) as avg_settlement_time_seconds,
    PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (updated_at - created_at))) as p50_settlement_time,
    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (updated_at - created_at))) as p95_settlement_time,
    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (updated_at - created_at))) as p99_settlement_time,
    SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed_count,
    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failed_count
FROM transactions
WHERE created_at >= NOW() - INTERVAL '30 days'
GROUP BY tenant_id, asset_code;

-- Create index for efficient querying
CREATE INDEX IF NOT EXISTS idx_mv_asset_perf_tenant
    ON mv_asset_performance_summary(tenant_id);

-- Create unlogged table to track last refresh time of each materialized view
CREATE TABLE IF NOT EXISTS materialized_view_refresh_log (
    view_name VARCHAR(255) PRIMARY KEY,
    last_refresh_at TIMESTAMP WITH TIME ZONE DEFAULT NOW(),
    refresh_duration_ms INT,
    row_count BIGINT
);

-- Update refresh log function (called after each refresh)
CREATE OR REPLACE FUNCTION log_materialized_view_refresh(
    view_name VARCHAR,
    duration_ms INT,
    row_count BIGINT
) RETURNS void AS $$
BEGIN
    INSERT INTO materialized_view_refresh_log (view_name, last_refresh_at, refresh_duration_ms, row_count)
    VALUES (view_name, NOW(), duration_ms, row_count)
    ON CONFLICT (view_name) DO UPDATE SET
        last_refresh_at = EXCLUDED.last_refresh_at,
        refresh_duration_ms = EXCLUDED.refresh_duration_ms,
        row_count = EXCLUDED.row_count;
END;
$$ LANGUAGE plpgsql;

-- Grant permissions to the application user for querying materialized views
GRANT SELECT ON mv_daily_transaction_volume TO synapse_user;
GRANT SELECT ON mv_daily_settlement_summary TO synapse_user;
GRANT SELECT ON mv_transaction_status_distribution TO synapse_user;
GRANT SELECT ON mv_hourly_request_volume TO synapse_user;
GRANT SELECT ON mv_asset_performance_summary TO synapse_user;
GRANT SELECT ON materialized_view_refresh_log TO synapse_user;
