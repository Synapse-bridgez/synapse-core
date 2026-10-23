-- #1437: optimistic concurrency, transactional outbox, and external event dedupe.
--
-- * `transactions.version` is bumped by a trigger on every status change, so
--   any writer (old or new code path) advances it. Status writers that must not
--   overwrite a concurrent change compare against it (see
--   `queries::update_transaction_status_versioned`).
-- * `event_outbox` rows are written in the same DB transaction as the state
--   change they describe. A background dispatcher publishes them and marks them
--   dispatched only after a successful publish (at-least-once delivery).
-- * External event dedupe records live in `idempotency_keys` (see
--   `queries::claim_external_event`) and expire via `expires_at`.

ALTER TABLE transactions
    ADD COLUMN IF NOT EXISTS version BIGINT NOT NULL DEFAULT 0;

CREATE OR REPLACE FUNCTION bump_transaction_version() RETURNS trigger AS $$
BEGIN
    NEW.version := OLD.version + 1;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER transactions_bump_version
    BEFORE UPDATE OF status ON transactions
    FOR EACH ROW
    WHEN (OLD.status IS DISTINCT FROM NEW.status)
    EXECUTE FUNCTION bump_transaction_version();

CREATE TABLE IF NOT EXISTS event_outbox (
    id BIGSERIAL PRIMARY KEY,
    tenant_id UUID,
    aggregate_id VARCHAR(255) NOT NULL,
    event_type VARCHAR(100) NOT NULL,
    payload JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    dispatched_at TIMESTAMPTZ
);

-- Partial index keeps the dispatcher's "pending rows" scan small.
CREATE INDEX IF NOT EXISTS idx_event_outbox_pending
    ON event_outbox(id) WHERE dispatched_at IS NULL;
