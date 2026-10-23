-- #1506: pre-aggregated swap analytics. Rows are fully recomputable from
-- swap_activity for any bucket (delete + insert per bucket), all in UTC.
CREATE TABLE IF NOT EXISTS swap_analytics_buckets (
    tenant_id             UUID        NOT NULL,
    granularity           TEXT        NOT NULL CHECK (granularity IN ('hour', 'day', 'month')),
    bucket_start          TIMESTAMPTZ NOT NULL,
    pair                  TEXT        NOT NULL,
    direction             TEXT        NOT NULL,
    status                TEXT        NOT NULL,
    order_count           BIGINT      NOT NULL,
    volume                NUMERIC     NOT NULL,
    avg_slippage_bps      NUMERIC,
    median_settlement_ms  DOUBLE PRECISION,
    computed_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, granularity, bucket_start, pair, direction, status)
);
CREATE INDEX IF NOT EXISTS idx_swap_analytics_tenant_bucket_pair
    ON swap_analytics_buckets (tenant_id, bucket_start, pair);
