-- No rollback needed: partitions are managed by maintain_partitions()
-- and detach_old_partitions(). Dropping them here could cause data loss.
SELECT 1;
