//! End-to-end latency budget for the webhook → reconciliation pipeline.
//!
//! Trace-ID propagation tells us *which* spans belong to one transaction;
//! this module answers *how much of the end-to-end SLA each stage spends*
//! and *which stage is closest to (or over) its share*.
//!
//! ## Budget
//!
//! Each [`PipelineStage`] gets a budget in milliseconds; the budgets sum to
//! the end-to-end SLA ([`LatencyBudget::sla_ms`]). Overrides come from
//! `LATENCY_BUDGET_<STAGE>_MS`; if `LATENCY_BUDGET_E2E_SLA_MS` is also set it
//! must equal the sum, so the per-stage shares and the SLA can never drift
//! apart silently.
//!
//! ## Attribution — derived from existing spans
//!
//! No bespoke timers: a stage's latency is the wall time of the tracing spans
//! mapped to it by [`stage_for_span`], **exclusive** of any nested span that
//! belongs to a *different* stage (so e.g. `processor.stage.validate` inside
//! `processor.process_transaction` counts toward validation, not twice).
//! Nested spans of the *same* stage are covered by their outermost ancestor.
//! A span may carry a `pipeline.queue_wait_ms` field for time spent queued
//! before the span began (the processor records how long a transaction sat
//! pending), which is added to that stage.
//!
//! The same rules are implemented twice: incrementally by
//! [`StageLatencyLayer`] for live traffic, and over recorded span data by
//! [`attribute_trace`] (used by tests against fixture traces and usable for
//! offline analysis of exported traces).
//!
//! ## Periodic stages
//!
//! Settlement (hourly) and reconciliation (daily) do not run per
//! transaction; for them a transaction's latency is dominated by *waiting
//! for the next run*. So:
//!
//! * [`attribute_trace`], which knows when the transaction became eligible,
//!   charges the stage `run.end - eligible_at` for the first run starting
//!   after eligibility.
//! * The live layer cannot link a run to individual transactions, so each
//!   run contributes one sample equal to the *expected* per-transaction
//!   contribution: run duration + half the observed interval since the
//!   previous run (the mean wait for transactions arriving uniformly), falling
//!   back to the stage's nominal interval for the first run.
//!
//! ## Evaluation & alerting
//!
//! [`LatencyBudgetTracker::evaluate`] runs once per evaluation window
//! (default 5 min): per stage it takes the P95 of the window's samples and
//! records over/under budget. A stage is *consistently* over budget — and
//! `PipelineLatencyBudgetExceeded` fires — when at least `breach_windows` of
//! its last `history_windows` evaluated windows were over (windows with no
//! samples are not counted, so a daily stage is judged over its last runs).
//! Reporting and alerting only; budgets are never rebalanced automatically.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::alerting::{names, AlertPayload, AlertSeverity};

/// Span field carrying queue time that precedes the span (milliseconds).
pub const QUEUE_WAIT_FIELD: &str = "pipeline.queue_wait_ms";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PipelineStage {
    Ingestion,
    Validation,
    Processing,
    Settlement,
    Reconciliation,
}

impl PipelineStage {
    /// In pipeline order.
    pub const ALL: [PipelineStage; 5] = [
        PipelineStage::Ingestion,
        PipelineStage::Validation,
        PipelineStage::Processing,
        PipelineStage::Settlement,
        PipelineStage::Reconciliation,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            PipelineStage::Ingestion => "ingestion",
            PipelineStage::Validation => "validation",
            PipelineStage::Processing => "processing",
            PipelineStage::Settlement => "settlement",
            PipelineStage::Reconciliation => "reconciliation",
        }
    }

    /// Stages that run on a schedule rather than per transaction.
    pub fn is_periodic(self) -> bool {
        self.nominal_interval().is_some()
    }

    /// The schedule of a periodic stage (settlement worker in `main.rs`,
    /// `ReconciliationJob::schedule`).
    pub fn nominal_interval(self) -> Option<Duration> {
        match self {
            PipelineStage::Settlement => Some(Duration::from_secs(3600)),
            PipelineStage::Reconciliation => Some(Duration::from_secs(86_400)),
            _ => None,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn env_key(self) -> String {
        format!("LATENCY_BUDGET_{}_MS", self.as_str().to_uppercase())
    }

    /// Default budget. Periodic stages are sized for their mean wait
    /// (half the interval) plus run time.
    pub fn default_budget_ms(self) -> f64 {
        match self {
            PipelineStage::Ingestion => 250.0,
            PipelineStage::Validation => 50.0,
            PipelineStage::Processing => 30_000.0,
            // 30 min mean wait for the hourly run + 10 min run.
            PipelineStage::Settlement => 2_400_000.0,
            // 12 h mean wait for the daily run + 30 min run.
            PipelineStage::Reconciliation => 45_000_000.0,
        }
    }
}

/// Maps an instrumented span name to the stage it measures.
pub fn stage_for_span(name: &str) -> Option<PipelineStage> {
    match name {
        "webhook.callback" | "webhook.transaction_callback" | "webhook.handle_webhook" => {
            Some(PipelineStage::Ingestion)
        }
        "webhook.validate" | "processor.stage.validate" => Some(PipelineStage::Validation),
        "processor.process_transaction" => Some(PipelineStage::Processing),
        "settlement.run" => Some(PipelineStage::Settlement),
        "reconciliation.run" => Some(PipelineStage::Reconciliation),
        _ => None,
    }
}

/// Per-stage budgets summing to the end-to-end SLA.
#[derive(Debug, Clone, PartialEq)]
pub struct LatencyBudget {
    budgets_ms: [f64; 5],
}

impl Default for LatencyBudget {
    fn default() -> Self {
        Self {
            budgets_ms: PipelineStage::ALL.map(PipelineStage::default_budget_ms),
        }
    }
}

impl LatencyBudget {
    /// Budgets in pipeline order. Every budget must be positive.
    pub fn new(budgets_ms: [f64; 5]) -> Result<Self, String> {
        for (stage, b) in PipelineStage::ALL.iter().zip(budgets_ms) {
            if !(b.is_finite() && b > 0.0) {
                return Err(format!("{} budget must be > 0, got {b}", stage.as_str()));
            }
        }
        Ok(Self { budgets_ms })
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let mut budgets = PipelineStage::ALL.map(PipelineStage::default_budget_ms);
        for stage in PipelineStage::ALL {
            let key = stage.env_key();
            if let Some(raw) = lookup(&key) {
                budgets[stage.index()] = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("{key}={raw:?} is not a number"))?;
            }
        }
        let budget = Self::new(budgets)?;
        if let Some(raw) = lookup("LATENCY_BUDGET_E2E_SLA_MS") {
            let sla: f64 = raw
                .trim()
                .parse()
                .map_err(|_| format!("LATENCY_BUDGET_E2E_SLA_MS={raw:?} is not a number"))?;
            if (sla - budget.sla_ms()).abs() > 0.5 {
                return Err(format!(
                    "stage budgets sum to {} ms but LATENCY_BUDGET_E2E_SLA_MS is {sla} ms",
                    budget.sla_ms()
                ));
            }
        }
        Ok(budget)
    }

    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    pub fn budget_ms(&self, stage: PipelineStage) -> f64 {
        self.budgets_ms[stage.index()]
    }

    /// The end-to-end SLA: the sum of all stage budgets.
    pub fn sla_ms(&self) -> f64 {
        self.budgets_ms.iter().sum()
    }
}

// ---------------------------------------------------------------------------
// Offline attribution over recorded spans
// ---------------------------------------------------------------------------

/// One recorded span (e.g. from an exported trace). Times in milliseconds on
/// any common clock.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanRecord {
    pub id: u64,
    pub parent: Option<u64>,
    pub name: String,
    pub start_ms: f64,
    pub end_ms: f64,
    /// Value of the `pipeline.queue_wait_ms` field, if recorded.
    pub queue_wait_ms: Option<f64>,
}

/// One execution of a periodic stage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodicRun {
    pub stage: PipelineStage,
    pub start_ms: f64,
    pub end_ms: f64,
}

/// Per-stage latency for one transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StageAttribution {
    pub per_stage_ms: BTreeMap<PipelineStage, f64>,
    /// Periodic stages the transaction has not been through yet (no run
    /// after it became eligible).
    pub pending: Vec<PipelineStage>,
}

impl StageAttribution {
    pub fn total_ms(&self) -> f64 {
        self.per_stage_ms.values().sum()
    }
}

/// Exclusive per-stage time for every stage-root span in `spans` (see the
/// module docs). Returns `(stage, ms)` samples; spans of periodic stages are
/// included too, so this also serves to time a run's own spans.
pub fn exclusive_stage_samples(spans: &[SpanRecord]) -> Vec<(PipelineStage, f64)> {
    let by_id: HashMap<u64, &SpanRecord> = spans.iter().map(|s| (s.id, s)).collect();
    let stage_of = |s: &SpanRecord| stage_for_span(&s.name);

    let nearest_staged_ancestor = |s: &SpanRecord| -> Option<&SpanRecord> {
        let mut cur = s.parent.and_then(|p| by_id.get(&p).copied());
        while let Some(a) = cur {
            if stage_of(a).is_some() {
                return Some(a);
            }
            cur = a.parent.and_then(|p| by_id.get(&p).copied());
        }
        None
    };
    let is_root = |s: &SpanRecord| match nearest_staged_ancestor(s) {
        None => true,
        Some(a) => stage_of(a) != stage_of(s),
    };
    let owner_root = |s: &SpanRecord| -> u64 {
        let mut cur = s;
        while !is_root(cur) {
            cur = nearest_staged_ancestor(cur).expect("non-root has a staged ancestor");
        }
        cur.id
    };

    let mut nested_other: HashMap<u64, f64> = HashMap::new();
    for s in spans.iter().filter(|s| stage_of(s).is_some() && is_root(s)) {
        if let Some(a) = nearest_staged_ancestor(s) {
            *nested_other.entry(owner_root(a)).or_default() += s.end_ms - s.start_ms;
        }
    }

    spans
        .iter()
        .filter_map(|s| {
            let stage = stage_of(s)?;
            if !is_root(s) {
                return None;
            }
            let own = (s.end_ms - s.start_ms) - nested_other.get(&s.id).copied().unwrap_or(0.0)
                + s.queue_wait_ms.unwrap_or(0.0);
            Some((stage, own.max(0.0)))
        })
        .collect()
}

/// Attributes one transaction's end-to-end latency to stages.
///
/// `spans` are the transaction's own spans (one trace); `runs` are
/// executions of the periodic stages around that time. A periodic stage is
/// charged from the moment the transaction became eligible for it (the end
/// of the previous stage) to the end of the first run starting at or after
/// that moment.
pub fn attribute_trace(spans: &[SpanRecord], runs: &[PeriodicRun]) -> StageAttribution {
    let mut out = StageAttribution::default();
    let per_tx: Vec<&SpanRecord> = spans
        .iter()
        .filter(|s| stage_for_span(&s.name).is_some_and(|st| !st.is_periodic()))
        .collect();

    for (stage, ms) in exclusive_stage_samples(spans) {
        if !stage.is_periodic() {
            *out.per_stage_ms.entry(stage).or_default() += ms;
        }
    }

    let Some(mut eligible_at) = per_tx.iter().map(|s| s.end_ms).reduce(f64::max) else {
        out.pending = PipelineStage::ALL
            .into_iter()
            .filter(|s| s.is_periodic())
            .collect();
        return out;
    };

    let periodic = PipelineStage::ALL.into_iter().filter(|s| s.is_periodic());
    for stage in periodic {
        if !out.pending.is_empty() {
            out.pending.push(stage);
            continue;
        }
        let next_run = runs
            .iter()
            .filter(|r| r.stage == stage && r.start_ms >= eligible_at)
            .min_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
        match next_run {
            Some(run) => {
                out.per_stage_ms.insert(stage, run.end_ms - eligible_at);
                eligible_at = run.end_ms;
            }
            None => out.pending.push(stage),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Live tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EvaluationConfig {
    /// Windows (out of the last `history_windows` evaluated) that must be
    /// over budget for the stage to count as consistently over.
    pub breach_windows: usize,
    pub history_windows: usize,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            breach_windows: 4,
            history_windows: 6,
        }
    }
}

impl EvaluationConfig {
    pub fn from_env() -> Self {
        fn env(key: &str) -> Option<usize> {
            std::env::var(key).ok()?.parse().ok()
        }
        let d = Self::default();
        let history = env("LATENCY_BUDGET_HISTORY_WINDOWS")
            .unwrap_or(d.history_windows)
            .max(1);
        Self {
            breach_windows: env("LATENCY_BUDGET_BREACH_WINDOWS")
                .unwrap_or(d.breach_windows)
                .clamp(1, history),
            history_windows: history,
        }
    }
}

/// One stage's result from an evaluation window.
#[derive(Debug, Clone, PartialEq)]
pub struct StageReport {
    pub stage: PipelineStage,
    pub budget_ms: f64,
    pub samples: usize,
    /// `None` if the window had no samples for this stage.
    pub p95_ms: Option<f64>,
    pub windows_over: usize,
    pub windows_evaluated: usize,
    /// Consistently over budget (the alert condition).
    pub breached: bool,
}

impl StageReport {
    pub fn utilization(&self) -> Option<f64> {
        self.p95_ms.map(|p| p / self.budget_ms)
    }

    pub fn to_alert(&self) -> AlertPayload {
        AlertPayload::new(
            names::PIPELINE_LATENCY_BUDGET_EXCEEDED,
            AlertSeverity::Warning,
            format!(
                "{} stage over its latency budget in {} of the last {} windows",
                self.stage.as_str(),
                self.windows_over,
                self.windows_evaluated
            ),
        )
        .with_label("stage", self.stage.as_str())
        .with_label("p95_ms", format!("{:.0}", self.p95_ms.unwrap_or(0.0)))
        .with_label("budget_ms", format!("{:.0}", self.budget_ms))
    }
}

#[derive(Default)]
struct TrackerState {
    samples: [Vec<f64>; 5],
    history: [VecDeque<bool>; 5],
    last_run_start: [Option<Instant>; 5],
    breached: [bool; 5],
    latest: Vec<StageReport>,
}

/// Collects per-stage samples and evaluates them against the budget.
pub struct LatencyBudgetTracker {
    budget: LatencyBudget,
    config: EvaluationConfig,
    state: Mutex<TrackerState>,
}

/// Result of one [`LatencyBudgetTracker::evaluate`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub reports: Vec<StageReport>,
    /// Stages that became consistently over budget in this evaluation.
    pub newly_breached: Vec<PipelineStage>,
    /// Stages that stopped being consistently over budget.
    pub recovered: Vec<PipelineStage>,
}

/// Samples kept per stage per window; beyond this the window is sampled
/// (every other observation dropped) so memory stays bounded under load.
const MAX_SAMPLES_PER_WINDOW: usize = 10_000;

impl LatencyBudgetTracker {
    pub fn new(budget: LatencyBudget, config: EvaluationConfig) -> Self {
        Self {
            budget,
            config,
            state: Mutex::new(TrackerState::default()),
        }
    }

    pub fn budget(&self) -> &LatencyBudget {
        &self.budget
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TrackerState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records one latency sample for a per-transaction stage.
    pub fn record(&self, stage: PipelineStage, ms: f64) {
        crate::metrics::pipeline_stage_latency_ms()
            .record(ms, &[opentelemetry::KeyValue::new("stage", stage.as_str())]);
        let mut st = self.lock();
        let samples = &mut st.samples[stage.index()];
        if samples.len() >= MAX_SAMPLES_PER_WINDOW {
            let mut i = 0;
            samples.retain(|_| {
                i += 1;
                i % 2 == 0
            });
        }
        samples.push(ms);
    }

    /// Records one run of a periodic stage that started at `started` and
    /// took `run_ms`; the sample is the expected per-transaction
    /// contribution (run time + mean wait for the run).
    pub fn record_periodic_run(&self, stage: PipelineStage, started: Instant, run_ms: f64) {
        let interval = {
            let mut st = self.lock();
            let prev = st.last_run_start[stage.index()].replace(started);
            prev.and_then(|p| started.checked_duration_since(p))
                .filter(|d| !d.is_zero())
                .or(stage.nominal_interval())
                .unwrap_or_default()
        };
        self.record(stage, run_ms + interval.as_secs_f64() * 1000.0 / 2.0);
    }

    /// Closes the current window: computes each stage's P95, updates the
    /// over-budget history and reports breach transitions.
    pub fn evaluate(&self) -> Evaluation {
        let mut guard = self.lock();
        let st = &mut *guard;
        let mut reports = Vec::with_capacity(5);
        let mut newly_breached = Vec::new();
        let mut recovered = Vec::new();

        for stage in PipelineStage::ALL {
            let i = stage.index();
            let budget_ms = self.budget.budget_ms(stage);
            let samples = std::mem::take(&mut st.samples[i]);
            let p95_ms = percentile(&samples, 0.95);

            if let Some(p95) = p95_ms {
                let history = &mut st.history[i];
                history.push_back(p95 > budget_ms);
                while history.len() > self.config.history_windows {
                    history.pop_front();
                }
            }
            let history = &st.history[i];
            let windows_over = history.iter().filter(|o| **o).count();
            let breached = windows_over >= self.config.breach_windows;

            if breached && !st.breached[i] {
                newly_breached.push(stage);
            } else if !breached && st.breached[i] {
                recovered.push(stage);
            }
            st.breached[i] = breached;

            reports.push(StageReport {
                stage,
                budget_ms,
                samples: samples.len(),
                p95_ms,
                windows_over,
                windows_evaluated: history.len(),
                breached,
            });
        }

        st.latest = reports.clone();
        Evaluation {
            reports,
            newly_breached,
            recovered,
        }
    }

    /// Reports from the most recent evaluation, or budget-only rows before
    /// the first one.
    pub fn latest(&self) -> Vec<StageReport> {
        let st = self.lock();
        if !st.latest.is_empty() {
            return st.latest.clone();
        }
        PipelineStage::ALL
            .into_iter()
            .map(|stage| StageReport {
                stage,
                budget_ms: self.budget.budget_ms(stage),
                samples: 0,
                p95_ms: None,
                windows_over: 0,
                windows_evaluated: 0,
                breached: false,
            })
            .collect()
    }
}

/// Nearest-rank percentile; `None` for no samples.
pub fn percentile(samples: &[f64], q: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (q * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// The process-wide tracker, configured from the environment. An invalid
/// budget configuration is logged and replaced by the defaults rather than
/// taking the service down.
pub fn global() -> &'static Arc<LatencyBudgetTracker> {
    static TRACKER: OnceLock<Arc<LatencyBudgetTracker>> = OnceLock::new();
    TRACKER.get_or_init(|| {
        let budget = LatencyBudget::from_env().unwrap_or_else(|e| {
            tracing::error!("invalid latency budget configuration ({e}); using defaults");
            LatencyBudget::default()
        });
        Arc::new(LatencyBudgetTracker::new(
            budget,
            EvaluationConfig::from_env(),
        ))
    })
}

/// Evaluates `tracker` every `interval`, emitting
/// `PipelineLatencyBudgetExceeded` when a stage becomes consistently over
/// budget. Runs until the process exits.
pub async fn run_evaluator(tracker: Arc<LatencyBudgetTracker>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    ticker.tick().await; // the first tick is immediate; wait a full window
    loop {
        ticker.tick().await;
        let evaluation = tracker.evaluate();
        for stage in &evaluation.newly_breached {
            if let Some(report) = evaluation.reports.iter().find(|r| r.stage == *stage) {
                report.to_alert().emit();
            }
        }
        for stage in &evaluation.recovered {
            tracing::info!(
                stage = stage.as_str(),
                "pipeline stage back within latency budget"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// tracing layer
// ---------------------------------------------------------------------------

struct StageTiming {
    stage: PipelineStage,
    /// Outermost span of its stage (see module docs).
    root: bool,
    started: Instant,
    nested_other_ms: f64,
    queue_wait_ms: f64,
}

struct QueueWaitVisitor(Option<f64>);

impl Visit for QueueWaitVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == QUEUE_WAIT_FIELD {
            self.0 = Some(value);
        }
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_f64(field, value as f64);
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_f64(field, value as f64);
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
}

/// `tracing` layer that turns closed pipeline spans into stage samples.
pub struct StageLatencyLayer {
    tracker: Arc<LatencyBudgetTracker>,
}

impl StageLatencyLayer {
    pub fn new(tracker: Arc<LatencyBudgetTracker>) -> Self {
        Self { tracker }
    }
}

/// The layer feeding the [`global`] tracker.
pub fn layer() -> StageLatencyLayer {
    StageLatencyLayer::new(Arc::clone(global()))
}

impl<S> Layer<S> for StageLatencyLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(stage) = stage_for_span(attrs.metadata().name()) else {
            return;
        };
        let Some(span) = ctx.span(id) else { return };
        let ancestor_stage = span
            .scope()
            .skip(1)
            .find_map(|a| a.extensions().get::<StageTiming>().map(|t| t.stage));
        let mut visitor = QueueWaitVisitor(None);
        attrs.record(&mut visitor);
        span.extensions_mut().insert(StageTiming {
            stage,
            root: ancestor_stage != Some(stage),
            started: Instant::now(),
            nested_other_ms: 0.0,
            queue_wait_ms: visitor.0.unwrap_or(0.0),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut ext = span.extensions_mut();
        if let Some(timing) = ext.get_mut::<StageTiming>() {
            let mut visitor = QueueWaitVisitor(None);
            values.record(&mut visitor);
            if let Some(q) = visitor.0 {
                timing.queue_wait_ms = q;
            }
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some((stage, root, started, nested_other_ms, queue_wait_ms)) =
            span.extensions().get::<StageTiming>().map(|t| {
                (
                    t.stage,
                    t.root,
                    t.started,
                    t.nested_other_ms,
                    t.queue_wait_ms,
                )
            })
        else {
            return;
        };
        if !root {
            return;
        }
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        // Charge this span's wall time out of the owning root of the
        // enclosing (different) stage.
        if let Some(owner) = span
            .scope()
            .skip(1)
            .find(|a| a.extensions().get::<StageTiming>().is_some_and(|t| t.root))
        {
            if let Some(t) = owner.extensions_mut().get_mut::<StageTiming>() {
                t.nested_other_ms += elapsed_ms;
            }
        }

        let own_ms = (elapsed_ms - nested_other_ms).max(0.0) + queue_wait_ms;
        if stage.is_periodic() {
            self.tracker.record_periodic_run(stage, started, own_ms);
        } else {
            self.tracker.record(stage, own_ms);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    fn span(id: u64, parent: Option<u64>, name: &str, start: f64, end: f64) -> SpanRecord {
        SpanRecord {
            id,
            parent,
            name: name.to_string(),
            start_ms: start,
            end_ms: end,
            queue_wait_ms: None,
        }
    }

    fn run(stage: PipelineStage, start: f64, end: f64) -> PeriodicRun {
        PeriodicRun {
            stage,
            start_ms: start,
            end_ms: end,
        }
    }

    /// A fixture trace with known stage boundaries:
    ///
    /// ```text
    /// t=0      http.request (unstaged)
    /// t=0..5     webhook.validate                 validation  5
    /// t=5..45    webhook.callback                 ingestion  40
    /// t=45     (pending in queue, recorded as 955 ms wait)
    /// t=1000..1300 processor.process_transaction  processing 300 - 20 + 955
    /// t=1010..1030   processor.stage.validate     validation 20
    /// t=1030..1100   processor.stage (verify)     (unstaged, stays in processing)
    /// settlement run 3000..3500 → 3500 - 1300 = 2200
    /// reconciliation runs 2000..2100 (too early), 10000..10400 → 10400 - 3500 = 6900
    /// ```
    fn fixture() -> (Vec<SpanRecord>, Vec<PeriodicRun>) {
        let mut process = span(4, None, "processor.process_transaction", 1000.0, 1300.0);
        process.queue_wait_ms = Some(955.0);
        let spans = vec![
            span(1, None, "http.request", 0.0, 45.0),
            span(2, Some(1), "webhook.validate", 0.0, 5.0),
            span(3, Some(1), "webhook.callback", 5.0, 45.0),
            span(10, Some(3), "db.insert_transaction", 10.0, 40.0),
            process,
            span(5, Some(4), "processor.stage.validate", 1010.0, 1030.0),
            span(6, Some(4), "processor.stage", 1030.0, 1100.0),
        ];
        let runs = vec![
            run(PipelineStage::Settlement, 500.0, 900.0),
            run(PipelineStage::Settlement, 3000.0, 3500.0),
            run(PipelineStage::Reconciliation, 2000.0, 2100.0),
            run(PipelineStage::Reconciliation, 10_000.0, 10_400.0),
            run(PipelineStage::Settlement, 6000.0, 6100.0),
        ];
        (spans, runs)
    }

    #[test]
    fn attributes_fixture_trace_to_known_stage_boundaries() {
        let (spans, runs) = fixture();
        let a = attribute_trace(&spans, &runs);
        assert_eq!(a.per_stage_ms[&PipelineStage::Ingestion], 40.0);
        assert_eq!(a.per_stage_ms[&PipelineStage::Validation], 25.0);
        assert_eq!(
            a.per_stage_ms[&PipelineStage::Processing],
            300.0 - 20.0 + 955.0
        );
        assert_eq!(a.per_stage_ms[&PipelineStage::Settlement], 2200.0);
        assert_eq!(a.per_stage_ms[&PipelineStage::Reconciliation], 6900.0);
        assert!(a.pending.is_empty());
        // Everything from the first span to reconciliation end, minus the
        // unattributed gap between ingestion end (45) and the recorded
        // queue wait start (1000 - 955 = 45) — i.e. none.
        assert_eq!(a.total_ms(), 10_400.0);
    }

    #[test]
    fn periodic_stages_pending_until_a_run_happens() {
        let (spans, runs) = fixture();
        let only_settlement: Vec<_> = runs
            .into_iter()
            .filter(|r| r.stage == PipelineStage::Settlement)
            .collect();
        let a = attribute_trace(&spans, &only_settlement);
        assert_eq!(a.pending, vec![PipelineStage::Reconciliation]);

        let a = attribute_trace(&spans, &[]);
        assert_eq!(
            a.pending,
            vec![PipelineStage::Settlement, PipelineStage::Reconciliation]
        );
        assert!(!a.per_stage_ms.contains_key(&PipelineStage::Settlement));

        let a = attribute_trace(&[], &[]);
        assert_eq!(a.pending.len(), 2);
        assert!(a.per_stage_ms.is_empty());
    }

    #[test]
    fn same_stage_nesting_is_not_double_counted() {
        // processing > (unstaged) > processing, with validation nested in
        // the inner processing span: validation time comes out of the
        // outer (root) processing span.
        let spans = vec![
            span(1, None, "processor.process_transaction", 0.0, 100.0),
            span(2, Some(1), "helper", 10.0, 90.0),
            span(3, Some(2), "processor.process_transaction", 20.0, 80.0),
            span(4, Some(3), "processor.stage.validate", 30.0, 40.0),
        ];
        let samples = exclusive_stage_samples(&spans);
        assert_eq!(
            samples,
            vec![
                (PipelineStage::Processing, 90.0),
                (PipelineStage::Validation, 10.0)
            ]
        );
    }

    #[test]
    fn span_name_mapping() {
        assert_eq!(
            stage_for_span("webhook.callback"),
            Some(PipelineStage::Ingestion)
        );
        assert_eq!(
            stage_for_span("webhook.handle_webhook"),
            Some(PipelineStage::Ingestion)
        );
        assert_eq!(
            stage_for_span("webhook.validate"),
            Some(PipelineStage::Validation)
        );
        assert_eq!(
            stage_for_span("settlement.run"),
            Some(PipelineStage::Settlement)
        );
        assert_eq!(
            stage_for_span("reconciliation.run"),
            Some(PipelineStage::Reconciliation)
        );
        assert_eq!(stage_for_span("webhook.get_transaction"), None);
    }

    #[test]
    fn default_budget_sums_to_sla() {
        let b = LatencyBudget::default();
        let sum: f64 = PipelineStage::ALL.iter().map(|s| b.budget_ms(*s)).sum();
        assert_eq!(b.sla_ms(), sum);
        for s in PipelineStage::ALL {
            assert!(b.budget_ms(s) > 0.0);
            assert_eq!(s.is_periodic(), s.nominal_interval().is_some());
        }
    }

    #[test]
    fn budget_from_env_overrides_and_checks_sla() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let b =
            LatencyBudget::from_lookup(env(&[("LATENCY_BUDGET_PROCESSING_MS", "1000")])).unwrap();
        assert_eq!(b.budget_ms(PipelineStage::Processing), 1000.0);

        let b = LatencyBudget::from_lookup(env(&[
            ("LATENCY_BUDGET_INGESTION_MS", "1"),
            ("LATENCY_BUDGET_VALIDATION_MS", "1"),
            ("LATENCY_BUDGET_PROCESSING_MS", "1"),
            ("LATENCY_BUDGET_SETTLEMENT_MS", "1"),
            ("LATENCY_BUDGET_RECONCILIATION_MS", "1"),
            ("LATENCY_BUDGET_E2E_SLA_MS", "5"),
        ]))
        .unwrap();
        assert_eq!(b.sla_ms(), 5.0);

        let err =
            LatencyBudget::from_lookup(env(&[("LATENCY_BUDGET_E2E_SLA_MS", "10")])).unwrap_err();
        assert!(err.contains("sum to"), "{err}");
        assert!(LatencyBudget::from_lookup(env(&[("LATENCY_BUDGET_INGESTION_MS", "x")])).is_err());
        assert!(LatencyBudget::from_lookup(env(&[("LATENCY_BUDGET_INGESTION_MS", "0")])).is_err());
        assert!(LatencyBudget::from_lookup(env(&[("LATENCY_BUDGET_E2E_SLA_MS", "?")])).is_err());
    }

    #[test]
    fn percentile_nearest_rank() {
        assert_eq!(percentile(&[], 0.95), None);
        assert_eq!(percentile(&[7.0], 0.95), Some(7.0));
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.95), Some(95.0));
        assert_eq!(percentile(&v, 0.5), Some(50.0));
    }

    fn tracker(breach: usize, history: usize) -> LatencyBudgetTracker {
        LatencyBudgetTracker::new(
            LatencyBudget::new([10.0, 10.0, 10.0, 10.0, 10.0]).unwrap(),
            EvaluationConfig {
                breach_windows: breach,
                history_windows: history,
            },
        )
    }

    #[test]
    fn alerts_only_when_consistently_over_budget() {
        let t = tracker(3, 5);
        let over = |t: &LatencyBudgetTracker| {
            for _ in 0..20 {
                t.record(PipelineStage::Processing, 50.0);
            }
            t.evaluate()
        };
        let under = |t: &LatencyBudgetTracker| {
            for _ in 0..20 {
                t.record(PipelineStage::Processing, 5.0);
            }
            t.evaluate()
        };

        // One spike is not "consistent".
        assert!(over(&t).newly_breached.is_empty());
        assert!(under(&t).newly_breached.is_empty());
        assert!(over(&t).newly_breached.is_empty());
        let e = over(&t);
        assert_eq!(e.newly_breached, vec![PipelineStage::Processing]);
        let report = &e.reports[PipelineStage::Processing.index()];
        assert_eq!(report.p95_ms, Some(50.0));
        assert_eq!(report.utilization(), Some(5.0));
        assert_eq!((report.windows_over, report.windows_evaluated), (3, 4));

        // Still breached: no duplicate transition.
        assert!(over(&t).newly_breached.is_empty());
        // Recovery once enough windows are back under budget.
        under(&t);
        under(&t);
        let e = under(&t);
        assert_eq!(e.recovered, vec![PipelineStage::Processing]);
        assert_eq!(t.latest(), e.reports);
    }

    #[test]
    fn empty_windows_do_not_count() {
        let t = tracker(2, 3);
        t.record(PipelineStage::Reconciliation, 100.0);
        t.evaluate();
        // Many empty windows between daily runs.
        for _ in 0..10 {
            let e = t.evaluate();
            assert_eq!(
                e.reports[PipelineStage::Reconciliation.index()].p95_ms,
                None
            );
        }
        t.record(PipelineStage::Reconciliation, 100.0);
        assert_eq!(
            t.evaluate().newly_breached,
            vec![PipelineStage::Reconciliation]
        );
    }

    #[test]
    fn periodic_run_charges_mean_wait() {
        let t = tracker(1, 1);
        let start = Instant::now();
        // First run: nominal hourly interval → 30 min mean wait.
        t.record_periodic_run(PipelineStage::Settlement, start, 1000.0);
        let e = t.evaluate();
        assert_eq!(
            e.reports[PipelineStage::Settlement.index()].p95_ms,
            Some(1000.0 + 1_800_000.0)
        );
        // Second run 10 minutes later → 5 min mean wait.
        t.record_periodic_run(
            PipelineStage::Settlement,
            start + Duration::from_secs(600),
            1000.0,
        );
        let e = t.evaluate();
        assert_eq!(
            e.reports[PipelineStage::Settlement.index()].p95_ms,
            Some(1000.0 + 300_000.0)
        );
    }

    #[test]
    fn latest_before_first_evaluation_has_budgets_only() {
        let t = tracker(1, 1);
        let rows = t.latest();
        assert_eq!(rows.len(), 5);
        assert!(rows
            .iter()
            .all(|r| r.p95_ms.is_none() && r.budget_ms == 10.0));
        assert_eq!(t.budget().sla_ms(), 50.0);
    }

    #[test]
    fn window_samples_are_bounded() {
        let t = tracker(1, 1);
        for _ in 0..(MAX_SAMPLES_PER_WINDOW * 3) {
            t.record(PipelineStage::Ingestion, 1.0);
        }
        let e = t.evaluate();
        assert!(e.reports[0].samples <= MAX_SAMPLES_PER_WINDOW);
    }

    #[test]
    fn report_renders_alert_with_runbook_link() {
        let t = tracker(1, 1);
        t.record(PipelineStage::Settlement, 99.0);
        let e = t.evaluate();
        let alert = e.reports[PipelineStage::Settlement.index()].to_alert();
        assert_eq!(alert.alert, names::PIPELINE_LATENCY_BUDGET_EXCEEDED);
        assert_eq!(alert.labels["stage"], "settlement");
        assert_eq!(alert.labels["p95_ms"], "99");
        assert_eq!(alert.labels["budget_ms"], "10");
        assert!(alert
            .runbook_url
            .unwrap()
            .ends_with("#pipeline-latency-budget-exceeded"));
    }

    #[test]
    fn evaluation_config_is_sane() {
        let c = EvaluationConfig::from_env();
        assert!(c.breach_windows >= 1 && c.breach_windows <= c.history_windows);
    }

    /// The live layer applies the same attribution rules to real spans.
    #[test]
    fn layer_attributes_live_spans() {
        let t = Arc::new(tracker(1, 1));
        let subscriber =
            tracing_subscriber::registry().with(StageLatencyLayer::new(Arc::clone(&t)));
        tracing::subscriber::with_default(subscriber, || {
            let sleep = |ms| std::thread::sleep(Duration::from_millis(ms));
            {
                let outer = tracing::info_span!(
                    "processor.process_transaction",
                    pipeline.queue_wait_ms = tracing::field::Empty
                );
                let _g = outer.enter();
                outer.record(QUEUE_WAIT_FIELD, 500_i64);
                sleep(20);
                {
                    let _v = tracing::info_span!("processor.stage.validate").entered();
                    sleep(40);
                }
                {
                    // Same-stage nesting is not recorded separately.
                    let _inner = tracing::info_span!("processor.process_transaction").entered();
                }
                let _unstaged = tracing::info_span!("processor.stage").entered();
            }
            {
                let _run = tracing::info_span!("settlement.run").entered();
            }
            let _ = tracing::info_span!("not.a.stage").entered();
        });

        let e = t.evaluate();
        let get = |s: PipelineStage| &e.reports[s.index()];
        assert_eq!(get(PipelineStage::Validation).samples, 1);
        assert_eq!(get(PipelineStage::Processing).samples, 1);
        let validation = get(PipelineStage::Validation).p95_ms.unwrap();
        let processing = get(PipelineStage::Processing).p95_ms.unwrap();
        assert!(
            (40.0..200.0).contains(&validation),
            "validation {validation}"
        );
        // 500 queue wait + ~20 own; the 40 ms validation is excluded.
        assert!(
            (520.0..650.0).contains(&processing),
            "processing {processing}"
        );
        let settlement = get(PipelineStage::Settlement).p95_ms.unwrap();
        assert!(settlement >= 1_800_000.0, "{settlement}");
        assert_eq!(get(PipelineStage::Ingestion).samples, 0);
    }
}
