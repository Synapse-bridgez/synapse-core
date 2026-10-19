-- #1435: Persisted swap quotes for the quoting engine (src/swap/quote.rs).
--
-- Each row is one issued quote. `candidates` holds the ranked, capped list of
-- per-source routes (best first) so a reviewer can see why a route won.
-- `request_key` is unique so repeated requests with the same key converge on
-- one quote.

CREATE TABLE IF NOT EXISTS swap_quotes (
    id           UUID        PRIMARY KEY,
    request_key  TEXT        NOT NULL UNIQUE,
    base_asset   TEXT        NOT NULL,
    quote_asset  TEXT        NOT NULL,
    amount       NUMERIC     NOT NULL CHECK (amount > 0),
    fee_bps      INTEGER     NOT NULL CHECK (fee_bps BETWEEN 0 AND 10000),
    candidates   JSONB       NOT NULL,
    issued_at    TIMESTAMPTZ NOT NULL,
    expires_at   TIMESTAMPTZ NOT NULL,
    CHECK (expires_at > issued_at)
);

CREATE INDEX IF NOT EXISTS idx_swap_quotes_expires_at ON swap_quotes (expires_at);
