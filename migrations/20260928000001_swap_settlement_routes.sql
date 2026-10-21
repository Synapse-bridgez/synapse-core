-- #1510: resolved Stellar destination + memo per swap order (reproducible, auditable).
-- #1531: legacy Phase 1 rows in `swap_ready` are intentionally and permanently left
-- as fiat-only transactions; nothing is migrated or dropped for them.
CREATE TABLE IF NOT EXISTS swap_settlement_routes (
    order_id    UUID        PRIMARY KEY,
    tenant_id   UUID        NOT NULL,
    destination TEXT        NOT NULL,
    memo_kind   TEXT        NOT NULL CHECK (memo_kind IN ('id', 'text')),
    memo        TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
-- Unique (destination, memo) guarantees one swap order per on-chain correlation key.
CREATE UNIQUE INDEX IF NOT EXISTS swap_settlement_routes_dest_memo_uq
    ON swap_settlement_routes (destination, memo_kind, memo);
