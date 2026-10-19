-- Rollback for 20260928000000_swap_partitioned_tables.
-- Dropping the parents also drops every attached partition. Partitions that were
-- already detached by retention are standalone tables (swap_*_yYYYYmMM) and are
-- intentionally left alone; drop them manually once archived.

DROP FUNCTION IF EXISTS detach_swap_partitions(INT);
DROP FUNCTION IF EXISTS ensure_swap_partitions(INT);
DROP FUNCTION IF EXISTS ensure_swap_partition_for(DATE);

DROP TABLE IF EXISTS swap_order_idempotency;
DROP TABLE IF EXISTS swap_events;
DROP TABLE IF EXISTS swap_fills;
DROP TABLE IF EXISTS swap_quotes;
DROP TABLE IF EXISTS swap_orders;
