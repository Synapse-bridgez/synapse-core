-- Drop the swap tables (child tables first; partitions drop with their parent)
-- and the swap-specific maintenance and trigger functions.

DROP TABLE IF EXISTS swap_events;
DROP TABLE IF EXISTS swap_fills;
DROP TABLE IF EXISTS swap_orders;
DROP TABLE IF EXISTS swap_quotes;

DROP FUNCTION IF EXISTS detach_old_swap_partitions(INTEGER);
DROP FUNCTION IF EXISTS ensure_swap_partitions(INT);
DROP FUNCTION IF EXISTS swap_events_reject_mutation();
