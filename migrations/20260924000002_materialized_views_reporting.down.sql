-- Drop materialized views and supporting objects
DROP FUNCTION IF EXISTS log_materialized_view_refresh(VARCHAR, INT, BIGINT);
DROP TABLE IF EXISTS materialized_view_refresh_log;
DROP MATERIALIZED VIEW IF EXISTS mv_asset_performance_summary;
DROP MATERIALIZED VIEW IF EXISTS mv_hourly_request_volume;
DROP MATERIALIZED VIEW IF EXISTS mv_transaction_status_distribution;
DROP MATERIALIZED VIEW IF EXISTS mv_daily_settlement_summary;
DROP MATERIALIZED VIEW IF EXISTS mv_daily_transaction_volume;
