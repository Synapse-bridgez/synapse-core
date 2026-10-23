-- Part C fix: ReconciliationJob was registered on JobScheduler with no
-- leader-election gating, and store_report was a plain INSERT with no
-- conflict handling — so if this job ever ran on more than one instance at
-- once, both would independently insert a full report row for the same
-- reconciliation window. This constraint makes a concurrent duplicate insert
-- collapse via ON CONFLICT DO NOTHING instead of succeeding twice, as a
-- backstop alongside the new LeaderElection gating in ReconciliationJob.
--
-- Requires period_start/period_end to be deterministic across concurrent
-- callers for the "same" scheduled run — ReconciliationJob::execute() now
-- truncates to the UTC day boundary instead of using per-instance Utc::now().

ALTER TABLE reconciliation_reports
    ADD CONSTRAINT reconciliation_reports_period_unique UNIQUE (period_start, period_end);
