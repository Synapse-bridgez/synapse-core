-- Async job tracking for bulk status updates that exceed the sync threshold.
-- Small batches continue to be processed inline; large batches are queued here
-- and executed by a background tokio task, with per-item results stored in
-- `result_summary` once the job completes.

CREATE TABLE IF NOT EXISTS bulk_status_jobs (
    id              UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    status          TEXT        NOT NULL DEFAULT 'pending'
                                CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    transaction_ids UUID[]      NOT NULL,
    target_status   TEXT        NOT NULL,
    reason          TEXT,
    actor           TEXT        NOT NULL DEFAULT 'admin',
    -- Per-item result summary populated on completion:
    -- [{ "transaction_id": "...", "ok": true/false, "error": "..." }, ...]
    result_summary  JSONB,
    error_message   TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_bulk_status_jobs_status
    ON bulk_status_jobs (status, created_at DESC);
