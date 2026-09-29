-- #1285: Transparent query access to cold-storage-tiered archived audit log partitions
--
-- Cold-storage tiering archives old audit_log rows to `audit_log_archives`.
-- This migration creates:
--   1. A `cold_audit_logs` table that holds rows promoted back from archive
--      storage for query purposes (cold data physically rehydrated on demand).
--   2. A `audit_logs_unified` view that transparently UNION ALLs the live
--      `audit_logs` table with `cold_audit_logs`, annotating each row with a
--      `tier` column ('hot' | 'cold') so callers can see which tier a row
--      came from without knowing the physical storage layout.
--   3. An `audit_log_cold_pointer` table that tracks which archive files have
--      been rehydrated into `cold_audit_logs`, so the application layer can
--      surface "cold storage touched" in API responses/logs and avoid
--      re-importing already-present data.
--
-- Out of scope: the archival/tiering mechanism itself (handled by run_retention
-- in src/db/audit.rs and the audit_log_archives table).

-- Holds rows rehydrated from cold archive files for unified query access.
-- Mirrors audit_logs schema exactly so the unified view can UNION ALL them.
CREATE TABLE IF NOT EXISTS cold_audit_logs (
    id           UUID         NOT NULL,
    entity_id    UUID         NOT NULL,
    entity_type  VARCHAR(50)  NOT NULL,
    action       VARCHAR(50)  NOT NULL,
    old_val      JSONB,
    new_val      JSONB,
    actor        VARCHAR(255) NOT NULL DEFAULT 'system',
    timestamp    TIMESTAMPTZ  NOT NULL,
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Origin archive reference so we know which archive file this row came from.
    archive_id   UUID         REFERENCES audit_log_archives(id) ON DELETE SET NULL,
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS idx_cold_audit_logs_entity_id
    ON cold_audit_logs(entity_id);

CREATE INDEX IF NOT EXISTS idx_cold_audit_logs_entity_type
    ON cold_audit_logs(entity_type);

CREATE INDEX IF NOT EXISTS idx_cold_audit_logs_timestamp
    ON cold_audit_logs(timestamp);

CREATE INDEX IF NOT EXISTS idx_cold_audit_logs_actor
    ON cold_audit_logs(actor);

CREATE INDEX IF NOT EXISTS idx_cold_audit_logs_archive_id
    ON cold_audit_logs(archive_id);

-- Tracks which archive files have been rehydrated into cold_audit_logs.
-- The application queries this table to decide whether a time-range query
-- touches cold storage (covers_from/covers_to overlap) and to avoid
-- double-importing rows from the same archive.
CREATE TABLE IF NOT EXISTS audit_log_cold_pointers (
    id           UUID         PRIMARY KEY DEFAULT gen_random_uuid(),
    archive_id   UUID         NOT NULL REFERENCES audit_log_archives(id) ON DELETE CASCADE,
    -- Timestamp range this rehydration covers (copied from audit_log_archives).
    covers_from  TIMESTAMPTZ  NOT NULL,
    covers_to    TIMESTAMPTZ  NOT NULL,
    -- When this archive file was loaded into cold_audit_logs.
    loaded_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
    -- Row count actually inserted from this archive.
    row_count    BIGINT       NOT NULL DEFAULT 0
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_cold_pointers_archive_id_unique
    ON audit_log_cold_pointers(archive_id);

CREATE INDEX IF NOT EXISTS idx_cold_pointers_covers
    ON audit_log_cold_pointers(covers_from, covers_to);

-- Unified view merging hot and cold audit log rows.
-- The `tier` column tells callers which physical store each row came from;
-- this is the only observable difference — the rest of the schema is identical.
CREATE OR REPLACE VIEW audit_logs_unified AS
    SELECT
        id,
        entity_id,
        entity_type,
        action,
        old_val,
        new_val,
        actor,
        timestamp,
        created_at,
        NULL::uuid   AS archive_id,
        'hot'::text  AS tier
    FROM audit_logs
  UNION ALL
    SELECT
        id,
        entity_id,
        entity_type,
        action,
        old_val,
        new_val,
        actor,
        timestamp,
        created_at,
        archive_id,
        'cold'::text AS tier
    FROM cold_audit_logs;

COMMENT ON VIEW audit_logs_unified IS
    'Unified read-only view merging live audit_logs (tier=hot) with rehydrated '
    'cold_audit_logs (tier=cold). Applications that query this view for a time '
    'range spanning cold data should check for tier=cold rows and surface the '
    'added latency expectation to callers.';
