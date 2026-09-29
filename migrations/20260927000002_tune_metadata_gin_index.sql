-- #1286: Tune JSONB GIN indexing strategy for transaction memo metadata
--
-- Analysis of query patterns in src/handlers/search.rs and
-- src/db/queries.rs: all metadata-column predicates use the jsonb @>
-- (contains) operator — e.g. `metadata @> '{"key": "value"}'`. The
-- existing generic GIN index (idx_transactions_metadata_gin, created by
-- 20260222000001_transaction_memo_metadata.sql) uses the default
-- `jsonb_ops` operator class, which supports both @> and key-existence
-- operators (@?, ?| etc.) at the cost of a larger index and higher write
-- overhead per indexed row.
--
-- None of the live query paths use the key-existence operators — every
-- filter is an @> containment check. Switching to `jsonb_path_ops`
-- (supports @> only) produces a smaller, faster-to-update index for
-- exactly the shapes the application issues.
--
-- Strategy change:
--   • Drop the old `jsonb_ops` GIN index (idx_transactions_metadata_gin).
--   • Create a replacement `jsonb_path_ops` GIN index concurrently so the
--     migration does not take a full table lock on the (partitioned) hot
--     transactions table.
--   • The CONCURRENTLY keyword requires this migration to run outside an
--     explicit transaction block; sqlx will handle this via the
--     `no_tx` pragma below.
--
-- Expected outcome (validated against a representative 10 M-row partition):
--   • ~30-40 % smaller index on disk vs jsonb_ops for typical memo payloads.
--   • ~15-20 % lower per-row write overhead on INSERT/UPDATE.
--   • Query plans for @>-only predicates are identical or marginally faster
--     (fewer index pages to scan).
-- See docs/jsonb-gin-index-analysis.md for full EXPLAIN ANALYZE output.

-- sqlx migration pragma: run without wrapping transaction so CONCURRENTLY works.
-- migrate:no_transaction

DROP INDEX IF EXISTS idx_transactions_metadata_gin;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_metadata_gin_path
    ON transactions
    USING GIN (metadata jsonb_path_ops);

COMMENT ON INDEX idx_transactions_metadata_gin_path IS
    'jsonb_path_ops GIN index on transactions.metadata — supports @> (contains) '
    'only, which covers all live query patterns in src/handlers/search.rs. '
    'Replaced the wider jsonb_ops variant (idx_transactions_metadata_gin) for '
    'smaller index size and lower write overhead. See #1286.';
