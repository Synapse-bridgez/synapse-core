DROP INDEX IF EXISTS idx_event_outbox_pending;
DROP TABLE IF EXISTS event_outbox;
DROP TRIGGER IF EXISTS transactions_bump_version ON transactions;
DROP FUNCTION IF EXISTS bump_transaction_version();
ALTER TABLE transactions DROP COLUMN IF EXISTS version;
