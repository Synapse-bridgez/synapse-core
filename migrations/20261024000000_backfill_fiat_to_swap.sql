-- Migration: Backfill state table for legacy fiat → Phase 2 swap domain (#1475)
--
-- Creates the checkpointing table used by the backfill job to record which
-- Phase 1 transaction rows have already been processed. This is the only
-- schema change the backfill requires; all target rows land in the existing
-- `swap_quotes`, `swap_orders`, `swap_fills`, `swap_events`, and
-- `swap_order_summary` tables which were created by earlier migrations.
--
-- Design notes:
--   * One row per source transaction id — unique constraint makes the
--     insert idempotent: ON CONFLICT DO NOTHING is the guard.
--   * `dry_run` flag records whether the row was created by a --dry-run pass
--     so those rows are excluded when real writes are needed.
--   * `backfill_run_id` ties every row to the specific invocation that wrote
--     it; useful when reviewing logs or rolling back a partial run.
--   * The table intentionally has no FK to `transactions` so it survives a
--     future partition detach without breaking referential integrity.

CREATE TABLE IF NOT EXISTS swap_backfill_state (
    -- Source Phase 1 transaction id (UUID, no FK — see design notes).
    transaction_id        UUID         NOT NULL,
    -- The Phase 2 swap order that was created for this transaction.
    -- NULL if --dry-run was in effect.
    swap_order_id         UUID,
    -- Set when the row was written by a --dry-run invocation.
    dry_run               BOOLEAN      NOT NULL DEFAULT FALSE,
    -- Opaque run identifier (e.g. ISO-8601 timestamp of job start).
    backfill_run_id       TEXT         NOT NULL,
    -- When the backfill processed this row.
    processed_at          TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    -- Human-readable status: 'created', 'skipped', 'error'.
    outcome               TEXT         NOT NULL DEFAULT 'created'
        CHECK (outcome IN ('created', 'skipped', 'error')),
    -- Populated when outcome = 'error'.
    error_detail          TEXT,

    PRIMARY KEY (transaction_id)
);

-- Lookup by run so operators can inspect or roll back a single pass.
CREATE INDEX IF NOT EXISTS idx_swap_backfill_state_run
    ON swap_backfill_state (backfill_run_id, processed_at);

-- Quick count of non-dry-run rows per outcome (used by progress logging).
CREATE INDEX IF NOT EXISTS idx_swap_backfill_state_outcome
    ON swap_backfill_state (outcome, dry_run);

COMMENT ON TABLE swap_backfill_state IS
    'Checkpoint table for the Phase 1 → Phase 2 swap domain backfill job (#1475). '
    'Each row records one processed Phase 1 transaction_id. '
    'The primary key provides idempotency: re-inserting a known id is a no-op.';
