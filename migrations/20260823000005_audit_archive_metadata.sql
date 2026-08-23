-- Part F fix: audit log retention wrote its gzip archive to local disk and
-- then unconditionally deleted the corresponding rows, with nothing
-- recording where a given period's archive actually lives, its checksum, or
-- which row IDs it covers. A future compliance/audit request had no way to
-- locate or verify a specific period's archive beyond guessing from the
-- filename convention. This table is the metadata record that
-- `run_retention` now writes in the same run it deletes rows, so a past
-- archive can always be located and verified.

CREATE TABLE audit_log_archives (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Human-readable location the archive was written to (e.g. a local path
    -- today; a bucket/key URI once a durable ArchiveStorage backend is wired
    -- in — see docs/audit_log_retention.md).
    location TEXT NOT NULL,
    checksum TEXT NOT NULL,
    row_count BIGINT NOT NULL,
    -- Timestamp range covered by this archive (inclusive of the oldest row,
    -- exclusive of the retention cutoff used for this run).
    covers_from TIMESTAMPTZ NOT NULL,
    covers_to TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_audit_log_archives_covers ON audit_log_archives (covers_from, covers_to);
