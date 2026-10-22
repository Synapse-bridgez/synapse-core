-- Create settlements table
CREATE TABLE IF NOT EXISTS settlements (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    asset_code VARCHAR(12) NOT NULL,
    total_amount NUMERIC NOT NULL,
    tx_count INTEGER NOT NULL,
    period_start TIMESTAMPTZ NOT NULL,
    period_end TIMESTAMPTZ NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- Add settlement_id to transactions table
ALTER TABLE transactions
ADD COLUMN IF NOT EXISTS settlement_id UUID REFERENCES settlements(id);
-- Create index for faster settlement lookups
CREATE INDEX IF NOT EXISTS idx_transactions_settlement_id ON transactions(settlement_id);

-- ============================================================================
-- Expand phase: dynamic network fee strategy & fee-bumping (#1484)
-- ============================================================================
-- All columns below are additive and nullable/defaulted so that running
-- replicas on the previous application version remain compatible during
-- rollout. Fee amounts are carried as i64 stroops end-to-end; NUMERIC is
-- used here only to avoid overflow when aggregating across many attempts.
-- See docs/migrations.md for the expand/contract policy.

-- Settlement attempts: record the fee actually paid per attempt so that
-- cost reporting and reconciliation can be performed later.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS actual_fee_stroops BIGINT NOT NULL DEFAULT 0;

-- Number of fee bumps applied to this attempt (0 = no bump needed).
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS fee_bump_count INTEGER NOT NULL DEFAULT 0;

-- Total stroops spent across the base fee plus all bumps for this attempt.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS total_fee_stroops BIGINT NOT NULL DEFAULT 0;

-- Classified reason when a fee cap rejected submission/bump, e.g.
-- 'per_tenant_cap_exceeded' or 'global_cap_exceeded'. NULL when no
-- rejection occurred.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS fee_cap_rejection_reason VARCHAR(64);

-- Horizon fee statistics snapshot used to derive the recommended fee.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS fee_percentile_used SMALLINT;

-- Timestamp of the most recent fee bump, if any.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS last_fee_bump_at TIMESTAMPTZ;

-- Index to support fee-cap rejection reporting and alerting queries.
CREATE INDEX IF NOT EXISTS idx_settlement_attempts_fee_cap_rejection
    ON settlement_attempts(fee_cap_rejection_reason)
    WHERE fee_cap_rejection_reason IS NOT NULL;

-- ============================================================================
-- Expand phase: trading calendars & settlement windows (#1499)
-- ============================================================================
-- Calendars are immutable and versioned so historical decisions can be
-- explained after the fact. New versions are inserted; existing rows are
-- never mutated.

CREATE TABLE IF NOT EXISTS trading_calendar_versions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_key VARCHAR(128) NOT NULL,
    version INTEGER NOT NULL,
    venue VARCHAR(64),
    corridor VARCHAR(64),
    timezone VARCHAR(64) NOT NULL,
    definition JSONB NOT NULL,
    effective_from TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT trading_calendar_versions_key_version_unique
        UNIQUE (calendar_key, version)
);

CREATE INDEX IF NOT EXISTS idx_trading_calendar_versions_key_effective
    ON trading_calendar_versions(calendar_key, effective_from DESC);

-- Record which calendar version governed a settlement attempt so that
-- scheduling decisions remain explainable after calendars change.
ALTER TABLE settlement_attempts
    ADD COLUMN IF NOT EXISTS calendar_version_id UUID
        REFERENCES trading_calendar_versions(id);