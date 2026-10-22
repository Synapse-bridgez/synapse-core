-- Rollback for 20260220000000_settlements (expand/contract practice).
-- This down path must be runnable against a populated database for rollback
-- rehearsal. It reverses the additive changes made by the forward migration:
--   * settlements table (and its indexes)
--   * transactions.settlement_id column and index
--   * fee-related columns added to transactions
--   * calendar-related columns/tables added by the forward migration
--
-- Ordering matters: drop dependent indexes before their columns/tables, and
-- drop columns before dropping any tables they reference. All statements are
-- idempotent (IF EXISTS) so the rehearsal can be re-run safely.

-- ---------------------------------------------------------------------------
-- Indexes on transactions (drop before the columns they cover)
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS idx_transactions_settlement_id;
DROP INDEX IF EXISTS idx_transactions_fee_asset;
DROP INDEX IF EXISTS idx_transactions_fee_calendar_id;

-- ---------------------------------------------------------------------------
-- Fee-related columns on transactions
-- ---------------------------------------------------------------------------
ALTER TABLE transactions DROP COLUMN IF EXISTS fee_amount;
ALTER TABLE transactions DROP COLUMN IF EXISTS fee_asset;
ALTER TABLE transactions DROP COLUMN IF EXISTS fee_calendar_id;

-- ---------------------------------------------------------------------------
-- Calendar-related columns on transactions
-- ---------------------------------------------------------------------------
ALTER TABLE transactions DROP COLUMN IF EXISTS calendar_id;
ALTER TABLE transactions DROP COLUMN IF EXISTS calendar_event_at;

-- ---------------------------------------------------------------------------
-- Settlement linkage on transactions
-- ---------------------------------------------------------------------------
ALTER TABLE transactions DROP COLUMN IF EXISTS settlement_id;

-- ---------------------------------------------------------------------------
-- Calendar tables (drop child/dependent tables before parents)
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS calendar_events;
DROP TABLE IF EXISTS calendars;

-- ---------------------------------------------------------------------------
-- Settlements table (drop after all referencing columns/indexes are gone)
-- ---------------------------------------------------------------------------
DROP TABLE IF EXISTS settlements;