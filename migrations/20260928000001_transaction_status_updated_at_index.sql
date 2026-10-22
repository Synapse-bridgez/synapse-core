-- #1446: Index for the stuck-transaction watchdog scan
--
-- src/db/queries.rs::find_stale_processing_ids filters on
-- (status = 'processing' AND updated_at < cutoff). Without this index every
-- watchdog tick would scan the whole partitioned transactions table.
--
-- Plain CREATE INDEX: CONCURRENTLY is not supported on partitioned tables.

CREATE INDEX IF NOT EXISTS idx_transactions_status_updated_at
    ON transactions (status, updated_at);
