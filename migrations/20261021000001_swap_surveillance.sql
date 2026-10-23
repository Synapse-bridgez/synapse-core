-- #1505: surveillance findings, reviewer audit log. Treated as sensitive
-- PII-adjacent data: admin-only access, purged after retention_until.
CREATE TABLE IF NOT EXISTS surveillance_findings (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    dedupe_key      TEXT        NOT NULL UNIQUE,
    rule            TEXT        NOT NULL,
    severity        TEXT        NOT NULL CHECK (severity IN ('low', 'medium', 'high', 'critical')),
    status          TEXT        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'acknowledged', 'dismissed')),
    tenant_ids      UUID[]      NOT NULL,
    accounts        TEXT[]      NOT NULL DEFAULT '{}',
    pair            TEXT,
    route           TEXT,
    window_start    TIMESTAMPTZ NOT NULL,
    window_end      TIMESTAMPTZ NOT NULL,
    evidence        JSONB       NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    retention_until TIMESTAMPTZ NOT NULL DEFAULT now() + interval '730 days'
);
CREATE INDEX IF NOT EXISTS idx_surv_findings_status ON surveillance_findings (status, severity, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_surv_findings_tenants ON surveillance_findings USING GIN (tenant_ids);

CREATE TABLE IF NOT EXISTS surveillance_audit_log (
    id          BIGSERIAL   PRIMARY KEY,
    finding_id  UUID        REFERENCES surveillance_findings(id) ON DELETE CASCADE,
    actor       TEXT        NOT NULL,
    action      TEXT        NOT NULL,
    from_status TEXT,
    to_status   TEXT,
    note        TEXT,
    at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_surv_audit_finding ON surveillance_audit_log (finding_id, at);
