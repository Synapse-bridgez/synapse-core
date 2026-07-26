-- Out-of-band index build for the live, high-volume `transactions` table.
--
-- migrations/20260222000000_transaction_search_indexes.sql and
-- migrations/20260427000000_optimized_search_indexes.sql create these same
-- indexes with plain `CREATE INDEX IF NOT EXISTS`, which takes a SHARE lock
-- and blocks concurrent INSERT/UPDATE/DELETE on `transactions` for the
-- duration of the build. `CREATE INDEX CONCURRENTLY` avoids that lock, but
-- cannot run inside a transaction block, and sqlx (0.7) wraps every
-- migration file in one with no opt-out — so it cannot be expressed as a
-- migration at all.
--
-- On any environment where `transactions` already carries production
-- traffic, run this script FIRST via psql (autocommit, one statement per
-- connection-transaction), e.g.:
--   psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f scripts/build_transaction_search_indexes_concurrently.sql
-- Once these indexes exist, the migrations' `IF NOT EXISTS` clauses make
-- their own CREATE INDEX statements no-ops. On a fresh/empty database the
-- migrations alone are sufficient and this script is unnecessary.

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_asset_code ON transactions(asset_code);
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_created_status ON transactions(created_at DESC, status);
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_amount ON transactions(amount);
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_created_id ON transactions(created_at DESC, id DESC);
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_search ON transactions(status, asset_code, created_at DESC);

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_status_asset_created ON transactions (status, asset_code, created_at DESC);
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_pending ON transactions (created_at DESC) WHERE status = 'pending';
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_transactions_metadata_gin ON transactions USING GIN (metadata jsonb_path_ops) WHERE metadata IS NOT NULL;
