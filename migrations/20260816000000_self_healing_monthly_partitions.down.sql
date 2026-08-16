-- No rollback needed: this migration only redefines create_monthly_partition()
-- to be self-healing and creates partitions (data-bearing tables). Dropping
-- partitions here could cause data loss; see 20260422000000_ensure_current_partitions.down.sql
-- for the same rationale.
SELECT 1;
