-- Settlement sequence management and transaction submission attempts.
--
-- This migration introduces durable, database-backed state for Stellar swap
-- settlement transactions:
--
--   * settlement_sequence_leases   - per source account (including channel
--                                    accounts) sequence allocation state that
--                                    survives process restarts and is safe
--                                    across multiple service replicas.
--   * settlement_sequence_allocations - individual allocated sequence numbers
--                                    (a lease may hold a contiguous block of
--                                    allocated sequences). Gaps are recoverable
--                                    by inspecting allocation status.
--   * settlement_submissions       - submission attempt records written BEFORE
--                                    the transaction is submitted so a crash
--                                    mid-submit leaves a recoverable record.
--                                    Used for duplicate detection (by tx hash)
--                                    and reconciliation against swap orders.
--
-- All timestamps are stored as TIMESTAMPTZ. Sequence numbers are stored as
-- BIGINT (Stellar sequence numbers are int64).

-- ---------------------------------------------------------------------------
-- settlement_sequence_leases
-- ---------------------------------------------------------------------------
-- One row per source account. Holds the high-water mark of sequence numbers
-- that have been handed out (or reserved) and the last sequence number known
-- to be confirmed on-chain. Allocation is performed under a row lock
-- (SELECT ... FOR UPDATE) so concurrent replicas cannot hand out the same
-- sequence number.
CREATE TABLE IF NOT EXISTS settlement_sequence_leases (
    source_account      TEXT        PRIMARY KEY,
    -- Highest sequence number that has been allocated (handed out) to a
    -- caller. New allocations start from GREATEST(allocated_through,
    -- confirmed_through) + 1.
    allocated_through   BIGINT      NOT NULL DEFAULT 0,
    -- Highest sequence number confirmed included on-chain. Used to recover
    -- from gaps and to reconcile after crashes.
    confirmed_through   BIGINT      NOT NULL DEFAULT 0,
    -- Number of sequence numbers currently reserved but not yet confirmed.
    -- Used to enforce per-account concurrency caps.
    in_flight_count     INTEGER     NOT NULL DEFAULT 0,
    -- Optional channel-account grouping. NULL for the primary source account.
    channel_account     TEXT,
    -- Lease metadata.
    lease_owner         TEXT,
    lease_expires_at    TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT settlement_sequence_leases_allocated_non_negative
        CHECK (allocated_through >= 0),
    CONSTRAINT settlement_sequence_leases_confirmed_non_negative
        CHECK (confirmed_through >= 0),
    CONSTRAINT settlement_sequence_leases_in_flight_non_negative
        CHECK (in_flight_count >= 0)
);

-- Reconciliation / channel-account pooling lookups.
CREATE INDEX IF NOT EXISTS idx_settlement_sequence_leases_channel_account
    ON settlement_sequence_leases (channel_account)
    WHERE channel_account IS NOT NULL;

-- Lease expiry sweeps (reclaim stale leases held by dead replicas).
CREATE INDEX IF NOT EXISTS idx_settlement_sequence_leases_lease_expires_at
    ON settlement_sequence_leases (lease_expires_at)
    WHERE lease_expires_at IS NOT NULL;

-- ---------------------------------------------------------------------------
-- settlement_sequence_allocations
-- ---------------------------------------------------------------------------
-- One row per allocated sequence number. Tracks the lifecycle of each
-- sequence number so that gaps (allocated but never submitted, or submitted
-- but never confirmed) are recoverable and so that duplicate submissions can
-- be detected.
CREATE TABLE IF NOT EXISTS settlement_sequence_allocations (
    id                  BIGSERIAL   PRIMARY KEY,
    source_account      TEXT        NOT NULL,
    sequence_number     BIGINT      NOT NULL,
    -- Lifecycle status:
    --   allocated   - handed out, not yet submitted
    --   submitted   - submission attempt recorded
    --   confirmed   - included on-chain
    --   failed      - terminal failure; sequence number is a gap
    --   expired     - reclaimed after lease expiry; sequence number is a gap
    status              TEXT        NOT NULL DEFAULT 'allocated',
    -- Optional linkage to the swap order / settlement attempt this sequence
    -- number was allocated for.
    swap_order_id       TEXT,
    -- Identifier of the replica/process that allocated the sequence number.
    allocated_by        TEXT,
    -- Transaction hash once known (after the envelope is built). Used for
    -- duplicate detection.
    transaction_hash    TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT settlement_sequence_allocations_status_valid
        CHECK (status IN ('allocated', 'submitted', 'confirmed', 'failed', 'expired')),
    CONSTRAINT settlement_sequence_allocations_unique_seq
        UNIQUE (source_account, sequence_number)
);

-- Reconciliation against swap orders.
CREATE INDEX IF NOT EXISTS idx_settlement_sequence_allocations_swap_order_id
    ON settlement_sequence_allocations (swap_order_id)
    WHERE swap_order_id IS NOT NULL;

-- Duplicate detection by transaction hash.
CREATE INDEX IF NOT EXISTS idx_settlement_sequence_allocations_transaction_hash
    ON settlement_sequence_allocations (transaction_hash)
    WHERE transaction_hash IS NOT NULL;

-- Gap recovery: find allocated-but-unresolved sequence numbers per account.
CREATE INDEX IF NOT EXISTS idx_settlement_sequence_allocations_status
    ON settlement_sequence_allocations (source_account, status);

-- ---------------------------------------------------------------------------
-- settlement_submissions
-- ---------------------------------------------------------------------------
-- One row per submission attempt. Written BEFORE the transaction is submitted
-- so a crash mid-submit leaves a recoverable record. Multiple attempts may
-- reference the same transaction hash (retries); the unique constraint on
-- (transaction_hash, attempt_number) keeps retries ordered and idempotent.
CREATE TABLE IF NOT EXISTS settlement_submissions (
    id                  BIGSERIAL   PRIMARY KEY,
    -- Linkage back to the allocation and swap order.
    allocation_id       BIGINT      REFERENCES settlement_sequence_allocations (id)
                                    ON DELETE SET NULL,
    source_account      TEXT        NOT NULL,
    sequence_number     BIGINT      NOT NULL,
    transaction_hash    TEXT        NOT NULL,
    swap_order_id       TEXT,
    -- Attempt ordering per transaction hash.
    attempt_number      INTEGER     NOT NULL DEFAULT 1,
    -- Submission lifecycle status:
    --   pending     - record written, submission in progress (crash-recoverable)
    --   submitted   - Horizon accepted the submission
    --   confirmed   - transaction included on-chain
    --   failed      - terminal failure (permanent error)
    --   retryable   - transient failure; a retry is scheduled
    --   unknown     - submission timed out; inclusion must be checked by hash
    status              TEXT        NOT NULL DEFAULT 'pending',
    -- Classified outcome of the submission attempt (e.g. tx_bad_seq,
    -- tx_too_low, tx_too_late, duplicate, timeout).
    outcome_code        TEXT,
    -- Human-readable detail. MUST NOT contain signed transaction envelopes.
    outcome_detail      TEXT,
    -- Ledger in which the transaction was included, once confirmed.
    ledger_sequence     BIGINT,
    -- Number of the retry attempt that produced this record (0 for the
    -- initial attempt).
    retry_count         INTEGER     NOT NULL DEFAULT 0,
    -- When the next retry is scheduled (bounded backoff).
    next_retry_at       TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT settlement_submissions_status_valid
        CHECK (status IN ('pending', 'submitted', 'confirmed', 'failed', 'retryable', 'unknown')),
    CONSTRAINT settlement_submissions_attempt_number_positive
        CHECK (attempt_number >= 1),
    CONSTRAINT settlement_submissions_retry_count_non_negative
        CHECK (retry_count >= 0),
    CONSTRAINT settlement_submissions_unique_attempt
        UNIQUE (transaction_hash, attempt_number)
);

-- Duplicate detection: look up submissions by transaction hash.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_transaction_hash
    ON settlement_submissions (transaction_hash);

-- Reconciliation against swap orders.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_swap_order_id
    ON settlement_submissions (swap_order_id)
    WHERE swap_order_id IS NOT NULL;

-- Reconciliation against allocations.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_allocation_id
    ON settlement_submissions (allocation_id)
    WHERE allocation_id IS NOT NULL;

-- Retry scheduling: find submissions due for retry.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_next_retry_at
    ON settlement_submissions (next_retry_at)
    WHERE next_retry_at IS NOT NULL;

-- Crash recovery: find submissions left in 'pending' or 'unknown' state.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_status
    ON settlement_submissions (status);

-- Per-account reconciliation and in-flight accounting.
CREATE INDEX IF NOT EXISTS idx_settlement_submissions_source_account
    ON settlement_submissions (source_account, status);

-- ---------------------------------------------------------------------------
-- Down migration
-- ---------------------------------------------------------------------------
-- Drop in reverse dependency order.
DROP INDEX IF EXISTS idx_settlement_submissions_source_account;
DROP INDEX IF EXISTS idx_settlement_submissions_status;
DROP INDEX IF EXISTS idx_settlement_submissions_next_retry_at;
DROP INDEX IF EXISTS idx_settlement_submissions_allocation_id;
DROP INDEX IF EXISTS idx_settlement_submissions_swap_order_id;
DROP INDEX IF EXISTS idx_settlement_submissions_transaction_hash;
DROP TABLE IF EXISTS settlement_submissions;

DROP INDEX IF EXISTS idx_settlement_sequence_allocations_status;
DROP INDEX IF EXISTS idx_settlement_sequence_allocations_transaction_hash;
DROP INDEX IF EXISTS idx_settlement_sequence_allocations_swap_order_id;
DROP TABLE IF EXISTS settlement_sequence_allocations;

DROP INDEX IF EXISTS idx_settlement_sequence_leases_lease_expires_at;
DROP INDEX IF EXISTS idx_settlement_sequence_leases_channel_account;
DROP TABLE IF EXISTS settlement_sequence_leases;