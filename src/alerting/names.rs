//! Catalog of every alert this service raises *in-process* (as opposed to
//! the Prometheus rules in `alerting/prometheus-rules.yml`, which are
//! evaluated by Prometheus itself).
//!
//! Every name here must appear in `alerting/runbook-links.json` with either
//! a runbook anchor or a reviewed exemption. That is enforced twice: by the
//! unit tests in `alerting::runbook`, and by
//! `scripts/check-alert-runbook-links.py` in CI, which extracts the
//! `pub const ...: &str = "...";` lines below with a regex — keep each
//! constant on a single line in exactly that form.

pub const SCHEDULED_JOB_MISSED_RUN: &str = "ScheduledJobMissedRun";
pub const SCHEDULED_JOB_FAILED: &str = "ScheduledJobFailed";
pub const PIPELINE_LATENCY_BUDGET_EXCEEDED: &str = "PipelineLatencyBudgetExceeded";
pub const TOKIO_TASK_LEAK_SUSPECTED: &str = "TokioTaskLeakSuspected";
pub const RELEASE_RELIABILITY_REGRESSION: &str = "ReleaseReliabilityRegression";
pub const QUOTA_CONFIG_SPLIT_BRAIN: &str = "QuotaConfigSplitBrain";

/// All in-process alert names, for exhaustive checks.
pub const ALL: &[&str] = &[
    SCHEDULED_JOB_MISSED_RUN,
    SCHEDULED_JOB_FAILED,
    PIPELINE_LATENCY_BUDGET_EXCEEDED,
    TOKIO_TASK_LEAK_SUSPECTED,
    RELEASE_RELIABILITY_REGRESSION,
    QUOTA_CONFIG_SPLIT_BRAIN,
];
