-- Stores EXPLAIN ANALYZE plans captured automatically when a query exceeds
-- the configured slow-query threshold.  Retention is bounded by a
-- background cleanup job (see src/db/slow_query.rs:prune_old_plans).
CREATE TABLE IF NOT EXISTS slow_query_plans (
    id            UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    captured_at   TIMESTAMPTZ NOT NULL    DEFAULT NOW(),
    query_name    TEXT        NOT NULL,
    query_sql     TEXT        NOT NULL,
    duration_ms   BIGINT      NOT NULL,
    threshold_ms  BIGINT      NOT NULL,
    rows_affected BIGINT      NOT NULL DEFAULT 0,
    plan_text     TEXT        NOT NULL
);

-- Recent slow queries first.
CREATE INDEX IF NOT EXISTS idx_slow_query_plans_captured_at
    ON slow_query_plans (captured_at DESC);

-- Efficient pruning by age.
CREATE INDEX IF NOT EXISTS idx_slow_query_plans_captured_at_asc
    ON slow_query_plans (captured_at ASC);

COMMENT ON TABLE slow_query_plans IS
    'Auto-captured EXPLAIN ANALYZE plans for queries that exceeded the slow-query threshold.  '
    'Pruned automatically after slow_query_plan_retention_days (default 30).';
