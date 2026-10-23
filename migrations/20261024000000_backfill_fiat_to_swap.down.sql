-- Rollback: remove backfill checkpoint table (#1475)
DROP TABLE IF EXISTS swap_backfill_state;
