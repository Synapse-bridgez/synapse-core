-- Dependency health scorecard (#1334): per-dependency, per-instance
-- 5-minute rollups of request outcomes, uptime minutes and latency bucket
-- counts. Written by services::dependency_scorecard's flush task, read by
-- /admin/dependencies/scorecard*. Not tenant data — no RLS.
--
-- latency_buckets holds counts for the fixed bucket layout
-- services::dependency_scorecard::LATENCY_BOUNDS_MS (+ overflow), so rows
-- merge across periods and instances by element-wise addition.
CREATE TABLE IF NOT EXISTS dependency_health_rollups (
    dependency               TEXT        NOT NULL
        CHECK (dependency IN ('settlement_api', 'redis', 'postgres', 'vault')),
    period_start             TIMESTAMPTZ NOT NULL,
    instance_id              TEXT        NOT NULL,
    calls                    BIGINT      NOT NULL DEFAULT 0,
    successes                BIGINT      NOT NULL DEFAULT 0,
    dependency_faults        BIGINT      NOT NULL DEFAULT 0,
    transport_failures       BIGINT      NOT NULL DEFAULT 0,
    circuit_rejected         BIGINT      NOT NULL DEFAULT 0,
    partition_failures       BIGINT      NOT NULL DEFAULT 0,
    minutes_observed         BIGINT      NOT NULL DEFAULT 0,
    minutes_up               BIGINT      NOT NULL DEFAULT 0,
    minutes_partitioned      BIGINT      NOT NULL DEFAULT 0,
    circuit_open_transitions BIGINT      NOT NULL DEFAULT 0,
    latency_buckets          BIGINT[]    NOT NULL DEFAULT '{}',
    recorded_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (dependency, period_start, instance_id)
);

CREATE INDEX IF NOT EXISTS idx_dependency_health_rollups_period
    ON dependency_health_rollups (period_start);
