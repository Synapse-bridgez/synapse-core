-- Swap Engine persistence (swap::pg_store::PgSwapStore).
--
-- Quotes and swap executions are stored as JSONB documents produced by the
-- engine's serde types. Every amount and rate inside them is a decimal
-- *string* with documented precision (see docs/swap-engine.md), so storing
-- the documents verbatim keeps exact values; nothing here is a float.

CREATE TABLE IF NOT EXISTS swap_quotes (
    quote_id    TEXT PRIMARY KEY,
    body        JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS swap_executions (
    swap_id     TEXT PRIMARY KEY,
    -- One execution per quote: the engine's idempotency guard.
    quote_id    TEXT        NOT NULL UNIQUE REFERENCES swap_quotes (quote_id),
    status      TEXT        NOT NULL,
    body        JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_swap_executions_status ON swap_executions (status);

CREATE TABLE IF NOT EXISTS swap_events (
    swap_id  TEXT        NOT NULL REFERENCES swap_executions (swap_id),
    seq      BIGINT      NOT NULL,
    at       TIMESTAMPTZ NOT NULL,
    kind     TEXT        NOT NULL,
    detail   JSONB       NOT NULL,
    PRIMARY KEY (swap_id, seq)
);
