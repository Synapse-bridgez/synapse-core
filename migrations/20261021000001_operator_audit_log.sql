-- Operator audit log for administrative swap actions (#1518).
--
-- Append-only and hash-chained: each row stores the SHA-256 of the previous
-- row's hash plus its own canonical content (computed in src/audit/mod.rs).
-- UPDATE, DELETE and TRUNCATE are rejected by triggers, and any edit made by
-- bypassing them (e.g. a superuser disabling triggers) breaks the chain and is
-- reported by verify_chain / the export endpoint.
--
-- Deliberately not partitioned and not covered by partition retention jobs,
-- so records survive the routine retention window; purge only by an explicit,
-- reviewed operation that re-anchors the chain.

CREATE TABLE IF NOT EXISTS operator_audit_log (
    seq           BIGSERIAL PRIMARY KEY,
    id            UUID        NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    occurred_at   TIMESTAMPTZ NOT NULL,
    actor         TEXT        NOT NULL,
    source        TEXT        NOT NULL,
    action        TEXT        NOT NULL,
    target        TEXT        NOT NULL,
    before_state  JSONB,
    after_state   JSONB,
    justification TEXT,
    prev_hash     TEXT        NOT NULL,
    hash          TEXT        NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS idx_operator_audit_log_occurred_at
    ON operator_audit_log (occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_operator_audit_log_actor
    ON operator_audit_log (actor, occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_operator_audit_log_action
    ON operator_audit_log (action, occurred_at DESC);

CREATE OR REPLACE FUNCTION operator_audit_log_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'operator_audit_log is append-only (% rejected)', TG_OP;
END;
$$;

DROP TRIGGER IF EXISTS operator_audit_log_no_update_delete ON operator_audit_log;
CREATE TRIGGER operator_audit_log_no_update_delete
    BEFORE UPDATE OR DELETE ON operator_audit_log
    FOR EACH ROW EXECUTE FUNCTION operator_audit_log_append_only();

DROP TRIGGER IF EXISTS operator_audit_log_no_truncate ON operator_audit_log;
CREATE TRIGGER operator_audit_log_no_truncate
    BEFORE TRUNCATE ON operator_audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION operator_audit_log_append_only();
