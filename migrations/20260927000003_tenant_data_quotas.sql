-- #1287: Tenant-level storage and row-count quota enforcement
--
-- Adds the database-side schema for per-tenant storage/row-count quotas.
-- The enforcement logic lives in src/services/tenant_data_quota.rs
-- (new scheduled job) and surfaces via the existing quota admin surface
-- at GET /admin/quotas and GET /admin/quotas/:tenant_id.
--
-- Soft threshold: warn in logs and surface in admin API; writes still allowed.
-- Hard threshold: block new INSERT rows from that tenant.

-- Per-tenant storage and row-count quota configuration.
CREATE TABLE IF NOT EXISTS tenant_data_quotas (
    tenant_id          UUID        PRIMARY KEY REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    -- Maximum number of transaction rows this tenant may have (NULL = unlimited).
    max_row_count      BIGINT,
    -- Maximum estimated storage in bytes across this tenant's partitions
    -- (NULL = unlimited).  Derived from pg_total_relation_size on the
    -- relevant partition(s); see tenant_data_quota_usage.
    max_storage_bytes  BIGINT,
    -- Fraction of the hard limit at which a soft warning is emitted
    -- (0.0–1.0, e.g. 0.8 = warn at 80 % of the hard limit).
    soft_threshold     NUMERIC(4, 3) NOT NULL DEFAULT 0.80
        CHECK (soft_threshold > 0 AND soft_threshold <= 1),
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Point-in-time snapshot of each tenant's measured usage, written by the
-- background quota-check job (tenant_data_quota_job).  A new row is inserted
-- on every run; the latest row per tenant is the current usage.
CREATE TABLE IF NOT EXISTS tenant_data_quota_usage (
    id                 UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          UUID        NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    -- Actual row count from `SELECT COUNT(*) FROM transactions WHERE tenant_id = $1`
    -- (or pg_class.reltuples for a fast approximate read).
    row_count          BIGINT      NOT NULL DEFAULT 0,
    -- Estimated storage in bytes from pg_total_relation_size aggregated over
    -- partitions belonging to this tenant (exact size is not partition-level
    -- granular in PG, so this is an overestimate shared across all tenants in
    -- that partition; the job records a per-tenant estimate).
    storage_bytes      BIGINT      NOT NULL DEFAULT 0,
    -- Derived from tenant_data_quotas at the time of measurement.
    -- NULL means no quota was configured for that dimension.
    row_count_pct      NUMERIC(6, 3),   -- 0–100+ (can exceed 100 when breached)
    storage_pct        NUMERIC(6, 3),
    -- Threshold breaches at measurement time.
    row_soft_breach    BOOLEAN     NOT NULL DEFAULT false,
    row_hard_breach    BOOLEAN     NOT NULL DEFAULT false,
    storage_soft_breach BOOLEAN    NOT NULL DEFAULT false,
    storage_hard_breach BOOLEAN    NOT NULL DEFAULT false,
    measured_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_tenant_data_quota_usage_tenant_measured
    ON tenant_data_quota_usage(tenant_id, measured_at DESC);

-- Convenience view: latest measured usage per tenant, joined with quota config.
CREATE OR REPLACE VIEW tenant_quota_summary AS
SELECT
    t.tenant_id,
    t.name                                             AS tenant_name,
    q.max_row_count,
    q.max_storage_bytes,
    q.soft_threshold,
    u.row_count,
    u.storage_bytes,
    u.row_count_pct,
    u.storage_pct,
    u.row_soft_breach,
    u.row_hard_breach,
    u.storage_soft_breach,
    u.storage_hard_breach,
    u.measured_at                                      AS last_measured_at
FROM tenants t
LEFT JOIN tenant_data_quotas  q ON q.tenant_id = t.tenant_id
LEFT JOIN LATERAL (
    SELECT *
    FROM   tenant_data_quota_usage u2
    WHERE  u2.tenant_id = t.tenant_id
    ORDER  BY u2.measured_at DESC
    LIMIT  1
) u ON true
WHERE t.is_active = true;

COMMENT ON TABLE tenant_data_quotas IS
    'Per-tenant storage and row-count quota configuration. '
    'Enforced by the background tenant_data_quota job. '
    'NULL limits mean "unlimited" for that dimension.';

COMMENT ON TABLE tenant_data_quota_usage IS
    'Time-series snapshots of each tenant''s actual data volume '
    'measured by the tenant_data_quota background job.';

COMMENT ON VIEW tenant_quota_summary IS
    'Latest measured usage per tenant joined with configured quota limits. '
    'Surfaced by GET /admin/quotas and GET /admin/quotas/:tenant_id.';
