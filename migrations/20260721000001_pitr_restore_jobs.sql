-- Tracks point-in-time-recovery (PITR) restore attempts as async jobs so the
-- CLI/admin API can submit a restore and poll for progress without blocking
-- the request thread on a long-running, destructive operation.
CREATE TABLE IF NOT EXISTS pitr_restore_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    target_timestamp TIMESTAMPTZ NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    dry_run BOOLEAN NOT NULL DEFAULT false,
    requested_by VARCHAR(255) NOT NULL,
    detail TEXT,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    CONSTRAINT chk_pitr_restore_jobs_status
        CHECK (status IN ('pending', 'running', 'succeeded', 'failed'))
);

CREATE INDEX idx_pitr_restore_jobs_status ON pitr_restore_jobs(status);
CREATE INDEX idx_pitr_restore_jobs_created_at ON pitr_restore_jobs(created_at DESC);
