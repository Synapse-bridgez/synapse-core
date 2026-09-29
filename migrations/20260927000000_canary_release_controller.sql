CREATE TABLE IF NOT EXISTS canary_release_state (
    release_name TEXT PRIMARY KEY,
    canary_traffic_percentage INTEGER NOT NULL DEFAULT 0 CHECK (canary_traffic_percentage BETWEEN 0 AND 100),
    flag_name TEXT NOT NULL,
    flag_rollout_percentage INTEGER NOT NULL DEFAULT 0 CHECK (flag_rollout_percentage BETWEEN 0 AND 100),
    traffic_rollback_threshold NUMERIC(8,5) NOT NULL DEFAULT 0.05000 CHECK (traffic_rollback_threshold >= 0),
    flag_rollback_threshold NUMERIC(8,5) NOT NULL DEFAULT 0.05000 CHECK (flag_rollback_threshold >= 0),
    traffic_error_rate NUMERIC(8,5) NOT NULL DEFAULT 0,
    flag_error_rate NUMERIC(8,5) NOT NULL DEFAULT 0,
    traffic_rollback_active BOOLEAN NOT NULL DEFAULT FALSE,
    flag_rollback_active BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_by TEXT NOT NULL DEFAULT 'system'
);

CREATE TABLE IF NOT EXISTS canary_release_audit (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    release_name TEXT NOT NULL REFERENCES canary_release_state(release_name) ON DELETE CASCADE,
    dimension TEXT NOT NULL CHECK (dimension IN ('traffic', 'flag')),
    old_percentage INTEGER NOT NULL,
    new_percentage INTEGER NOT NULL,
    action TEXT NOT NULL,
    error_rate NUMERIC(8,5),
    actor TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS canary_release_audit_release_created_idx
    ON canary_release_audit (release_name, created_at DESC);
