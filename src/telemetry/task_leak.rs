//! Tokio task leak detection.
//!
//! A leaked task (e.g. a WebSocket send loop whose sibling was never
//! aborted, or a receiver on a channel whose sender is never dropped) never
//! fails loudly — it just parks forever, and the process slowly accumulates
//! them over days until something runs out of memory or file descriptors.
//!
//! This module makes that growth visible and alertable:
//!
//! * **Tagged spawns.** Long-lived tasks are spawned through [`spawn_tracked`]
//!   with a [`TaskCategory`], which keeps a per-category live-task count. The
//!   count is decremented by a drop guard, so it stays correct whether the
//!   task completes, panics or is aborted.
//! * **Load signal.** Each category also tracks the load that *should*
//!   explain its task count ([`LoadGuard`] / [`TaskRegistry::set_load`]) —
//!   open WebSocket connections, registered scheduler jobs — and declares how
//!   many tasks one unit of load legitimately owns
//!   ([`TaskCategory::tasks_per_load`]).
//! * **Load-correlated detection.** [`LeakDetector`] never looks at the raw
//!   count: it tracks the *excess* `live - tasks_per_load * load`, which stays
//!   flat for a healthy pool no matter how much traffic grows, and flags a
//!   category only when that excess has grown steadily across the detection
//!   window.
//!
//! Metrics (see `src/metrics.rs`): `tokio_tasks_live{category}`,
//! `tokio_tasks_load{category}`, `tokio_tasks_spawned_total{category}`,
//! `tokio_task_leak_suspected_total{category}`, plus
//! `tokio_runtime_alive_tasks` from tokio's own runtime metrics so untagged
//! tasks are not invisible. Alert: `TokioTaskLeakSuspected` (runbook section
//! "Tokio Task Leak Suspected").

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::alerting::{names, AlertPayload, AlertSeverity};

/// Origin of a spawned task. Keep this set small and bounded — it is used as
/// a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskCategory {
    /// Per-connection WebSocket recv/send tasks (`handlers::ws`).
    WsConnection,
    /// Scheduler job loops (`services::scheduler`).
    SchedulerJob,
}

impl TaskCategory {
    pub const ALL: [TaskCategory; 2] = [TaskCategory::WsConnection, TaskCategory::SchedulerJob];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskCategory::WsConnection => "ws_connection",
            TaskCategory::SchedulerJob => "scheduler_job",
        }
    }

    /// How many long-lived tasks one unit of this category's load owns:
    /// every WebSocket connection runs one recv and one send task; every
    /// registered scheduler job runs one loop task.
    pub fn tasks_per_load(self) -> f64 {
        match self {
            TaskCategory::WsConnection => 2.0,
            TaskCategory::SchedulerJob => 1.0,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Default)]
struct CategoryCounters {
    live: AtomicU64,
    load: AtomicU64,
    spawned: AtomicU64,
}

/// Live-task and load counters for every [`TaskCategory`]. The process uses
/// the [`global`] registry; tests create their own so they don't interfere.
#[derive(Default)]
pub struct TaskRegistry {
    counters: [CategoryCounters; TaskCategory::ALL.len()],
}

/// A point-in-time reading for one category.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TaskSnapshot {
    pub category: TaskCategory,
    pub live: u64,
    pub load: u64,
    pub spawned_total: u64,
}

impl TaskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn counters(&self, category: TaskCategory) -> &CategoryCounters {
        &self.counters[category.index()]
    }

    /// Spawns `future` on the current tokio runtime, counted under `category`
    /// until it completes, panics or is aborted.
    pub fn spawn<F>(
        self: &Arc<Self>,
        category: TaskCategory,
        future: F,
    ) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let guard = self.enter(category);
        tokio::spawn(async move {
            let _guard = guard;
            future.await
        })
    }

    fn enter(self: &Arc<Self>, category: TaskCategory) -> LiveGuard {
        let counters = self.counters(category);
        counters.live.fetch_add(1, Ordering::Relaxed);
        counters.spawned.fetch_add(1, Ordering::Relaxed);
        LiveGuard {
            registry: Arc::clone(self),
            category,
        }
    }

    /// Increments `category`'s load for as long as the returned guard lives
    /// (e.g. one guard per open WebSocket connection).
    pub fn track_load(self: &Arc<Self>, category: TaskCategory) -> LoadGuard {
        self.counters(category).load.fetch_add(1, Ordering::Relaxed);
        LoadGuard {
            registry: Arc::clone(self),
            category,
        }
    }

    /// Sets `category`'s load outright, for loads that are a known count
    /// rather than a set of scoped resources (e.g. registered jobs).
    pub fn set_load(&self, category: TaskCategory, load: u64) {
        self.counters(category).load.store(load, Ordering::Relaxed);
    }

    pub fn snapshot(&self, category: TaskCategory) -> TaskSnapshot {
        let c = self.counters(category);
        TaskSnapshot {
            category,
            live: c.live.load(Ordering::Relaxed),
            load: c.load.load(Ordering::Relaxed),
            spawned_total: c.spawned.load(Ordering::Relaxed),
        }
    }

    pub fn snapshots(&self) -> Vec<TaskSnapshot> {
        TaskCategory::ALL
            .iter()
            .map(|c| self.snapshot(*c))
            .collect()
    }
}

struct LiveGuard {
    registry: Arc<TaskRegistry>,
    category: TaskCategory,
}

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.registry
            .counters(self.category)
            .live
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// Scoped unit of load; see [`TaskRegistry::track_load`].
pub struct LoadGuard {
    registry: Arc<TaskRegistry>,
    category: TaskCategory,
}

impl Drop for LoadGuard {
    fn drop(&mut self) {
        self.registry
            .counters(self.category)
            .load
            .fetch_sub(1, Ordering::Relaxed);
    }
}

/// The process-wide registry.
pub fn global() -> &'static Arc<TaskRegistry> {
    static REGISTRY: OnceLock<Arc<TaskRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Arc::new(TaskRegistry::new()))
}

/// [`TaskRegistry::spawn`] on the [`global`] registry.
pub fn spawn_tracked<F>(category: TaskCategory, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    global().spawn(category, future)
}

/// [`TaskRegistry::track_load`] on the [`global`] registry.
pub fn track_load(category: TaskCategory) -> LoadGuard {
    global().track_load(category)
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// Tuning for [`LeakDetector`]. Defaults suit a 1-minute sampling interval:
/// a leak has to add at least 50 unexplained tasks, fairly steadily, over
/// the last hour before it is reported.
#[derive(Debug, Clone)]
pub struct LeakDetectorConfig {
    /// Samples kept per category (the detection window).
    pub window: usize,
    /// Minimum samples before any verdict is given.
    pub min_samples: usize,
    /// Minimum growth in excess tasks (live beyond what load explains)
    /// between the start and end of the window.
    pub min_excess_growth: f64,
    /// Fraction of sample-to-sample steps in which the excess must not
    /// decrease. Leaks ratchet upward; healthy churn goes both ways.
    pub min_monotonic_fraction: f64,
}

impl Default for LeakDetectorConfig {
    fn default() -> Self {
        Self {
            window: 60,
            min_samples: 30,
            min_excess_growth: 50.0,
            min_monotonic_fraction: 0.8,
        }
    }
}

impl LeakDetectorConfig {
    /// Reads `TASK_LEAK_WINDOW_SAMPLES`, `TASK_LEAK_MIN_SAMPLES`,
    /// `TASK_LEAK_MIN_EXCESS_GROWTH` and `TASK_LEAK_MIN_MONOTONIC_FRACTION`,
    /// falling back to the defaults for anything unset or unparsable.
    pub fn from_env() -> Self {
        fn env<T: std::str::FromStr>(key: &str) -> Option<T> {
            std::env::var(key).ok()?.parse().ok()
        }
        let d = Self::default();
        Self {
            window: env("TASK_LEAK_WINDOW_SAMPLES").unwrap_or(d.window).max(2),
            min_samples: env("TASK_LEAK_MIN_SAMPLES").unwrap_or(d.min_samples).max(2),
            min_excess_growth: env("TASK_LEAK_MIN_EXCESS_GROWTH").unwrap_or(d.min_excess_growth),
            min_monotonic_fraction: env("TASK_LEAK_MIN_MONOTONIC_FRACTION")
                .unwrap_or(d.min_monotonic_fraction),
        }
    }
}

/// A category whose task count is growing faster than its load explains.
#[derive(Debug, Clone, PartialEq)]
pub struct LeakSuspicion {
    pub category: TaskCategory,
    /// Unexplained tasks at the start and end of the window.
    pub excess_start: f64,
    pub excess_end: f64,
    pub live: u64,
    pub load: u64,
    /// Fraction of steps in which the excess did not decrease.
    pub monotonic_fraction: f64,
}

impl LeakSuspicion {
    pub fn to_alert(&self) -> AlertPayload {
        AlertPayload::new(
            names::TOKIO_TASK_LEAK_SUSPECTED,
            AlertSeverity::Warning,
            format!(
                "{} tasks growing without matching load: {:.0} unexplained tasks now, {:.0} at start of window",
                self.category.as_str(),
                self.excess_end,
                self.excess_start
            ),
        )
        .with_label("category", self.category.as_str())
        .with_label("live", self.live)
        .with_label("load", self.load)
        .with_label("excess_growth", format!("{:.0}", self.excess_end - self.excess_start))
    }
}

/// Sliding-window detector over [`TaskSnapshot`]s. Feed it one snapshot per
/// category per interval with [`LeakDetector::observe`].
pub struct LeakDetector {
    config: LeakDetectorConfig,
    samples: HashMap<TaskCategory, VecDeque<TaskSnapshot>>,
}

impl LeakDetector {
    pub fn new(config: LeakDetectorConfig) -> Self {
        Self {
            config,
            samples: HashMap::new(),
        }
    }

    /// Records a sample and returns a suspicion if the category's window now
    /// shows load-uncorrelated growth.
    pub fn observe(&mut self, snapshot: TaskSnapshot) -> Option<LeakSuspicion> {
        let window = self.samples.entry(snapshot.category).or_default();
        window.push_back(snapshot);
        while window.len() > self.config.window {
            window.pop_front();
        }
        evaluate(&self.config, window)
    }
}

fn excess(s: &TaskSnapshot) -> f64 {
    s.live as f64 - s.category.tasks_per_load() * s.load as f64
}

fn evaluate(config: &LeakDetectorConfig, window: &VecDeque<TaskSnapshot>) -> Option<LeakSuspicion> {
    if window.len() < config.min_samples.max(2) {
        return None;
    }
    let excesses: Vec<f64> = window.iter().map(excess).collect();
    let first = excesses[0];
    let last = *excesses.last()?;
    if last - first < config.min_excess_growth {
        return None;
    }
    let steps = excesses.len() - 1;
    let non_decreasing = excesses.windows(2).filter(|w| w[1] >= w[0]).count();
    let monotonic_fraction = non_decreasing as f64 / steps as f64;
    if monotonic_fraction < config.min_monotonic_fraction {
        return None;
    }
    let latest = window.back()?;
    Some(LeakSuspicion {
        category: latest.category,
        excess_start: first,
        excess_end: last,
        live: latest.live,
        load: latest.load,
        monotonic_fraction,
    })
}

/// Alert state on top of [`LeakDetector`]: reports each leak episode once,
/// when it starts, and notes when it clears.
pub struct LeakMonitor {
    detector: LeakDetector,
    alerting: HashMap<TaskCategory, Instant>,
}

/// What changed in one [`LeakMonitor::tick`].
#[derive(Debug, Default, PartialEq)]
pub struct MonitorTick {
    pub started: Vec<LeakSuspicion>,
    /// Categories whose suspicion cleared, with how long it lasted.
    pub cleared: Vec<(TaskCategory, Duration)>,
}

impl LeakMonitor {
    pub fn new(config: LeakDetectorConfig) -> Self {
        Self {
            detector: LeakDetector::new(config),
            alerting: HashMap::new(),
        }
    }

    pub fn tick(&mut self, snapshots: impl IntoIterator<Item = TaskSnapshot>) -> MonitorTick {
        let mut out = MonitorTick::default();
        for snapshot in snapshots {
            let category = snapshot.category;
            match self.detector.observe(snapshot) {
                Some(suspicion) => {
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        self.alerting.entry(category)
                    {
                        e.insert(Instant::now());
                        out.started.push(suspicion);
                    }
                }
                None => {
                    if let Some(since) = self.alerting.remove(&category) {
                        out.cleared.push((category, since.elapsed()));
                    }
                }
            }
        }
        out
    }
}

/// Samples `registry` every `interval`, emitting `TokioTaskLeakSuspected`
/// once when a category starts looking leaky and logging when it recovers.
/// Runs until the process exits.
pub async fn run_leak_monitor(
    registry: Arc<TaskRegistry>,
    config: LeakDetectorConfig,
    interval: Duration,
) {
    let mut monitor = LeakMonitor::new(config);
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let tick = monitor.tick(registry.snapshots());
        for suspicion in tick.started {
            crate::metrics::tokio_task_leak_suspected_total().add(
                1,
                &[opentelemetry::KeyValue::new(
                    "category",
                    suspicion.category.as_str(),
                )],
            );
            suspicion.to_alert().emit();
        }
        for (category, lasted) in tick.cleared {
            tracing::info!(
                category = category.as_str(),
                alerting_for_secs = lasted.as_secs(),
                "tokio task leak suspicion cleared"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(window: usize, min_growth: f64) -> LeakDetectorConfig {
        LeakDetectorConfig {
            window,
            min_samples: window,
            min_excess_growth: min_growth,
            min_monotonic_fraction: 0.8,
        }
    }

    fn snap(category: TaskCategory, live: u64, load: u64) -> TaskSnapshot {
        TaskSnapshot {
            category,
            live,
            load,
            spawned_total: live,
        }
    }

    #[test]
    fn category_labels_are_stable() {
        assert_eq!(TaskCategory::WsConnection.as_str(), "ws_connection");
        assert_eq!(TaskCategory::SchedulerJob.as_str(), "scheduler_job");
        assert_eq!(TaskCategory::ALL.len(), 2);
    }

    #[test]
    fn raw_growth_that_tracks_load_is_not_a_leak() {
        // Traffic ramps from 10 to 500 connections: tasks go 20 -> 1000, but
        // every one of them is explained by load.
        let mut d = LeakDetector::new(cfg(20, 10.0));
        let mut verdict = None;
        for i in 0..20u64 {
            let load = 10 + i * 25;
            verdict = d.observe(snap(TaskCategory::WsConnection, load * 2, load));
        }
        assert_eq!(verdict, None);
    }

    #[test]
    fn growth_with_flat_load_is_a_leak() {
        let mut d = LeakDetector::new(cfg(20, 10.0));
        let mut verdict = None;
        for i in 0..20u64 {
            verdict = d.observe(snap(TaskCategory::WsConnection, 100 + i * 3, 50));
        }
        let s = verdict.expect("leak should be flagged");
        assert_eq!(s.category, TaskCategory::WsConnection);
        assert_eq!(s.excess_start, 0.0);
        assert_eq!(s.excess_end, 57.0);
        assert_eq!(s.monotonic_fraction, 1.0);
    }

    #[test]
    fn growth_while_load_falls_is_a_leak() {
        let mut d = LeakDetector::new(cfg(10, 10.0));
        let mut verdict = None;
        for i in 0..10u64 {
            // Load drains 100 -> 55 but tasks stay at 200.
            verdict = d.observe(snap(TaskCategory::WsConnection, 200, 100 - i * 5));
        }
        assert!(verdict.is_some());
    }

    #[test]
    fn noisy_churn_is_not_a_leak() {
        // Excess oscillates (tasks lagging behind load changes) with a net
        // upward drift, but not steadily.
        let mut d = LeakDetector::new(cfg(20, 10.0));
        let mut verdict = None;
        for i in 0..20u64 {
            let jitter = if i % 2 == 0 { 0 } else { 30 };
            verdict = d.observe(snap(TaskCategory::SchedulerJob, 5 + jitter + i, 5));
        }
        assert_eq!(verdict, None);
    }

    #[test]
    fn small_growth_below_threshold_is_ignored() {
        let mut d = LeakDetector::new(cfg(10, 50.0));
        let mut verdict = None;
        for i in 0..10u64 {
            verdict = d.observe(snap(TaskCategory::SchedulerJob, 5 + i, 5));
        }
        assert_eq!(verdict, None);
    }

    #[test]
    fn needs_min_samples_and_keeps_bounded_window() {
        let mut d = LeakDetector::new(LeakDetectorConfig {
            window: 5,
            min_samples: 5,
            min_excess_growth: 1.0,
            min_monotonic_fraction: 0.8,
        });
        for i in 0..4u64 {
            assert_eq!(d.observe(snap(TaskCategory::SchedulerJob, i * 10, 0)), None);
        }
        assert!(d.observe(snap(TaskCategory::SchedulerJob, 40, 0)).is_some());
        // Old samples fall out: a flat stretch clears the verdict.
        for _ in 0..5 {
            d.observe(snap(TaskCategory::SchedulerJob, 40, 0));
        }
        assert_eq!(d.observe(snap(TaskCategory::SchedulerJob, 40, 0)), None);
        assert_eq!(d.samples[&TaskCategory::SchedulerJob].len(), 5);
    }

    #[test]
    fn suspicion_renders_alert_with_runbook_link() {
        let s = LeakSuspicion {
            category: TaskCategory::WsConnection,
            excess_start: 0.0,
            excess_end: 80.0,
            live: 180,
            load: 50,
            monotonic_fraction: 0.95,
        };
        let alert = s.to_alert();
        assert_eq!(alert.alert, names::TOKIO_TASK_LEAK_SUSPECTED);
        assert_eq!(alert.labels["category"], "ws_connection");
        assert_eq!(alert.labels["excess_growth"], "80");
        assert!(alert
            .runbook_url
            .as_deref()
            .unwrap()
            .ends_with("#tokio-task-leak-suspected"));
    }

    #[test]
    fn env_config_falls_back_to_defaults() {
        let c = LeakDetectorConfig::from_env();
        assert!(c.window >= 2 && c.min_samples >= 2);
    }

    #[tokio::test]
    async fn tracked_tasks_are_counted_until_they_finish_or_abort() {
        let registry = Arc::new(TaskRegistry::new());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let done = registry.spawn(TaskCategory::SchedulerJob, async move {
            let _ = rx.await;
        });
        let parked = registry.spawn(TaskCategory::SchedulerJob, std::future::pending::<()>());
        let s = registry.snapshot(TaskCategory::SchedulerJob);
        assert_eq!((s.live, s.spawned_total), (2, 2));

        tx.send(()).unwrap();
        done.await.unwrap();
        assert_eq!(registry.snapshot(TaskCategory::SchedulerJob).live, 1);

        parked.abort();
        let _ = parked.await;
        assert_eq!(registry.snapshot(TaskCategory::SchedulerJob).live, 0);
        assert_eq!(
            registry.snapshot(TaskCategory::SchedulerJob).spawned_total,
            2
        );
    }

    #[tokio::test]
    async fn panicking_task_is_uncounted() {
        let registry = Arc::new(TaskRegistry::new());
        let h = registry.spawn(TaskCategory::WsConnection, async { panic!("boom") });
        assert!(h.await.is_err());
        assert_eq!(registry.snapshot(TaskCategory::WsConnection).live, 0);
    }

    #[test]
    fn load_guards_and_set_load() {
        let registry = Arc::new(TaskRegistry::new());
        let a = registry.track_load(TaskCategory::WsConnection);
        let b = registry.track_load(TaskCategory::WsConnection);
        assert_eq!(registry.snapshot(TaskCategory::WsConnection).load, 2);
        drop(a);
        drop(b);
        assert_eq!(registry.snapshot(TaskCategory::WsConnection).load, 0);
        registry.set_load(TaskCategory::SchedulerJob, 7);
        assert_eq!(registry.snapshot(TaskCategory::SchedulerJob).load, 7);
        assert_eq!(registry.snapshots().len(), 2);
    }

    /// End-to-end harness: a well-behaved connection handler next to one
    /// with a deliberate leak (its send task is never aborted when the
    /// connection closes — the classic ws.rs bug shape). Load is identical
    /// for both; only the leaky one must be flagged.
    #[tokio::test]
    async fn detects_deliberately_introduced_leak() {
        async fn simulate(leaky: bool) -> Option<LeakSuspicion> {
            let registry = Arc::new(TaskRegistry::new());
            let mut detector = LeakDetector::new(cfg(30, 20.0));
            let mut connections: VecDeque<(LoadGuard, Vec<tokio::task::JoinHandle<()>>)> =
                VecDeque::new();
            let mut verdict = None;

            for tick in 0..30 {
                // Steady state: 3 connections open and 3 close every tick,
                // so load stays flat at ~20 connections.
                for _ in 0..3 {
                    let load = registry.track_load(TaskCategory::WsConnection);
                    let recv = registry.spawn(TaskCategory::WsConnection, std::future::pending());
                    let send = registry.spawn(TaskCategory::WsConnection, std::future::pending());
                    connections.push_back((load, vec![recv, send]));
                }
                if tick > 5 {
                    for _ in 0..3 {
                        let (load, mut handles) = connections.pop_front().unwrap();
                        drop(load);
                        // The leak: the buggy handler aborts recv but
                        // forgets send, which stays parked forever.
                        if leaky {
                            handles.truncate(1);
                        }
                        for h in handles {
                            h.abort();
                            let _ = h.await;
                        }
                    }
                }
                tokio::task::yield_now().await;
                verdict = detector.observe(registry.snapshot(TaskCategory::WsConnection));
            }
            verdict
        }

        assert_eq!(
            simulate(false).await,
            None,
            "healthy handler must not be flagged"
        );
        let s = simulate(true).await.expect("leaky handler must be flagged");
        assert_eq!(s.category, TaskCategory::WsConnection);
        assert!(s.excess_end - s.excess_start >= 20.0, "{s:?}");
    }

    #[test]
    fn monitor_reports_each_episode_once() {
        let mut m = LeakMonitor::new(LeakDetectorConfig {
            window: 3,
            min_samples: 3,
            min_excess_growth: 2.0,
            min_monotonic_fraction: 0.8,
        });
        let feed = |m: &mut LeakMonitor, live| m.tick([snap(TaskCategory::SchedulerJob, live, 0)]);

        assert_eq!(feed(&mut m, 0), MonitorTick::default());
        assert_eq!(feed(&mut m, 1), MonitorTick::default());
        let t = feed(&mut m, 2);
        assert_eq!(t.started.len(), 1);
        assert_eq!(t.started[0].category, TaskCategory::SchedulerJob);
        // Still growing: same episode, no second report.
        assert!(feed(&mut m, 3).started.is_empty());
        // Growth across the window ([2, 3, 3]) drops below threshold:
        // cleared once.
        let t = feed(&mut m, 3);
        assert_eq!(t.cleared.len(), 1);
        assert_eq!(t.cleared[0].0, TaskCategory::SchedulerJob);
        assert_eq!(feed(&mut m, 3), MonitorTick::default());
    }

    #[tokio::test]
    async fn run_leak_monitor_samples_until_aborted() {
        let registry = Arc::new(TaskRegistry::new());
        let monitor = tokio::spawn(run_leak_monitor(
            Arc::clone(&registry),
            LeakDetectorConfig {
                window: 3,
                min_samples: 3,
                min_excess_growth: 1.0,
                min_monotonic_fraction: 0.5,
            },
            Duration::from_millis(5),
        ));
        let mut leaked = Vec::new();
        for _ in 0..10 {
            leaked.push(registry.spawn(TaskCategory::SchedulerJob, std::future::pending::<()>()));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!monitor.is_finished());
        monitor.abort();
        for h in leaked {
            h.abort();
        }
    }
}
