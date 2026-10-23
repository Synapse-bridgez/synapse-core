//! Bounded priority admission. Active requests are never preempted.
use std::{collections::VecDeque, sync::{Arc, Mutex}, time::{Duration, Instant}};
use axum::{extract::State, http::Request, middleware::Next, response::{IntoResponse, Response}};
use once_cell::sync::Lazy;
use prometheus::{Gauge, IntCounterVec, IntGaugeVec, Opts};
use tokio::sync::oneshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority { Mutation, Quote, Analytics }
impl Priority {
    fn index(self) -> usize { match self { Self::Mutation => 0, Self::Quote => 1, Self::Analytics => 2 } }
    fn label(self) -> &'static str { ["mutation", "quote", "analytics"][self.index()] }
}

#[derive(Clone, Debug)]
pub struct AdmissionConfig {
    pub enabled: bool,
    pub max_in_flight: usize,
    pub mutation_reserved: usize,
    pub queue_depth: [usize; 3],
    pub wait_timeout: [Duration; 3],
    pub shed_saturation: f64,
    pub latency_target: Duration,
}
impl Default for AdmissionConfig {
    fn default() -> Self {
        Self { enabled: true, max_in_flight: 64, mutation_reserved: 16,
            queue_depth: [128, 64, 16],
            wait_timeout: [Duration::from_secs(2), Duration::from_millis(250), Duration::from_millis(50)],
            shed_saturation: 0.9, latency_target: Duration::from_millis(500) }
    }
}
impl AdmissionConfig {
    pub fn from_env() -> Result<Self, String> {
        fn read<T: std::str::FromStr>(key: &str, default: T) -> Result<T, String> {
            match std::env::var(key) { Ok(v) => v.parse().map_err(|_| format!("invalid {key}")),
                Err(std::env::VarError::NotPresent) => Ok(default), Err(_) => Err(format!("invalid {key}")) }
        }
        let mut c = Self::default();
        c.enabled = read("SWAP_BACKPRESSURE_ENABLED", c.enabled)?;
        c.max_in_flight = read("SWAP_BACKPRESSURE_MAX_IN_FLIGHT", c.max_in_flight)?;
        c.mutation_reserved = read("SWAP_BACKPRESSURE_MUTATION_RESERVED", c.mutation_reserved)?;
        c.shed_saturation = read("SWAP_BACKPRESSURE_SHED_SATURATION", c.shed_saturation)?;
        c.latency_target = Duration::from_millis(read("SWAP_BACKPRESSURE_LATENCY_TARGET_MS", 500u64)?);
        for (i, tier) in ["MUTATION", "QUOTE", "ANALYTICS"].iter().enumerate() {
            c.queue_depth[i] = read(&format!("SWAP_BACKPRESSURE_{tier}_QUEUE_DEPTH"), c.queue_depth[i])?;
            c.wait_timeout[i] = Duration::from_millis(read(&format!("SWAP_BACKPRESSURE_{tier}_WAIT_MS"), c.wait_timeout[i].as_millis() as u64)?);
        }
        c.validate()?;
        Ok(c)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.max_in_flight == 0 || self.mutation_reserved >= self.max_in_flight
            || !self.shed_saturation.is_finite() || self.shed_saturation <= 0.0 || self.shed_saturation > 1.0
            || self.latency_target.is_zero() || self.wait_timeout.iter().any(Duration::is_zero) {
            return Err("invalid swap backpressure capacity, threshold, or timeout".into());
        }
        Ok(())
    }
}

struct Metrics { active: IntGaugeVec, queued: IntGaugeVec, rejected: IntCounterVec, saturation: Gauge }
static METRICS: Lazy<Metrics> = Lazy::new(|| {
    let active = IntGaugeVec::new(Opts::new("swap_admission_in_flight", "Admitted swap work"), &["tier"]).unwrap();
    let queued = IntGaugeVec::new(Opts::new("swap_admission_queue_depth", "Waiting swap work"), &["tier"]).unwrap();
    let rejected = IntCounterVec::new(Opts::new("swap_admission_rejected_total", "Rejected swap work"), &["tier", "reason"]).unwrap();
    let saturation = Gauge::new("swap_admission_saturation", "Fraction of total swap admission capacity in use").unwrap();
    prometheus::default_registry().register(Box::new(active.clone())).expect("unique admission metric");
    prometheus::default_registry().register(Box::new(queued.clone())).expect("unique admission metric");
    prometheus::default_registry().register(Box::new(rejected.clone())).expect("unique admission metric");
    prometheus::default_registry().register(Box::new(saturation.clone())).expect("unique admission metric");
    Metrics { active, queued, rejected, saturation }
});

struct Waiter { id: u64, ready: oneshot::Sender<()> }
struct Inner {
    active: [usize; 3],
    queues: [VecDeque<Waiter>; 3],
    next_id: u64,
    latency_ewma: f64,
    last_completion: Instant,
}
pub struct AdmissionController { config: AdmissionConfig, inner: Mutex<Inner> }
impl AdmissionController {
    pub fn new(config: AdmissionConfig) -> Result<Arc<Self>, String> {
        config.validate()?;
        Lazy::force(&METRICS);
        Ok(Arc::new(Self { config, inner: Mutex::new(Inner {
            active: [0; 3], queues: std::array::from_fn(|_| VecDeque::new()), next_id: 0,
            latency_ewma: 0.0, last_completion: Instant::now(),
        }) }))
    }
    fn capacity(&self, inner: &Inner, tier: usize) -> bool {
        inner.active.iter().sum::<usize>() < self.config.max_in_flight &&
            (tier == 0 || inner.active[1] + inner.active[2] < self.config.max_in_flight - self.config.mutation_reserved)
    }
    fn dispatch(&self, inner: &mut Inner) {
        for tier in 0..3 {
            while self.capacity(inner, tier) {
                let Some(waiter) = inner.queues[tier].pop_front() else { break };
                METRICS.queued.with_label_values(&[label(tier)]).dec();
                // Sending while holding the lock prevents cancellation racing with accounting.
                // Even a dropped receiver still has its Waiting guard, which
                // releases this grant after acquiring the same mutex.
                inner.active[tier] += 1;
                METRICS.active.with_label_values(&[label(tier)]).inc();
                let _ = waiter.ready.send(());
            }
            if !inner.queues[tier].is_empty() { break; }
        }
        METRICS.saturation.set(inner.active.iter().sum::<usize>() as f64 / self.config.max_in_flight as f64);
    }
    fn reject(tier: Priority, reason: &'static str) -> crate::error::SwapApiError {
        METRICS.rejected.with_label_values(&[tier.label(), reason]).inc();
        crate::error::SwapApiError::Unavailable
    }
    pub async fn acquire(self: &Arc<Self>, priority: Priority) -> Result<AdmissionPermit, crate::error::SwapApiError> {
        let tier = priority.index();
        if !self.config.enabled { return Ok(AdmissionPermit { controller: None, tier, started: Instant::now() }); }
        let (mut waiting, rx) = {
            let mut inner = self.inner.lock().unwrap();
            let total = inner.active.iter().sum::<usize>();
            let saturated = total as f64 / self.config.max_in_flight as f64 >= self.config.shed_saturation;
            // Old latency observations expire, allowing recovery after traffic stops.
            let slow = inner.last_completion.elapsed() < Duration::from_secs(5)
                && inner.latency_ewma > self.config.latency_target.as_secs_f64();
            if tier > 0 && (saturated || slow) { return Err(Self::reject(priority, "adaptive")); }
            if self.capacity(&inner, tier) && inner.queues[..=tier].iter().all(VecDeque::is_empty) {
                inner.active[tier] += 1;
                METRICS.active.with_label_values(&[priority.label()]).inc();
                METRICS.saturation.set((total + 1) as f64 / self.config.max_in_flight as f64);
                return Ok(AdmissionPermit { controller: Some(self.clone()), tier, started: Instant::now() });
            }
            if inner.queues[tier].len() >= self.config.queue_depth[tier] { return Err(Self::reject(priority, "queue_full")); }
            let id = inner.next_id;
            inner.next_id = inner.next_id.wrapping_add(1);
            let (tx, rx) = oneshot::channel();
            inner.queues[tier].push_back(Waiter { id, ready: tx });
            METRICS.queued.with_label_values(&[priority.label()]).inc();
            (Waiting { controller: self.clone(), tier, id, armed: true }, rx)
        };
        match tokio::time::timeout(self.config.wait_timeout[tier], rx).await {
            Ok(Ok(())) => {
                waiting.armed = false;
                Ok(AdmissionPermit { controller: Some(self.clone()), tier, started: Instant::now() })
            }
            _ => Err(Self::reject(priority, "timeout")),
        }
    }
}
fn label(tier: usize) -> &'static str { ["mutation", "quote", "analytics"][tier] }

// A granted-but-not-yet-polled waiter also owns an active slot. Removal must
// release that slot on cancellation/timeout, even when the queue entry is gone.
struct Waiting { controller: Arc<AdmissionController>, tier: usize, id: u64, armed: bool }
impl Drop for Waiting {
    fn drop(&mut self) {
        if !self.armed { return; }
        let mut inner = self.controller.inner.lock().unwrap();
        if let Some(index) = inner.queues[self.tier].iter().position(|w| w.id == self.id) {
            inner.queues[self.tier].remove(index);
            METRICS.queued.with_label_values(&[label(self.tier)]).dec();
        } else {
            inner.active[self.tier] -= 1;
            METRICS.active.with_label_values(&[label(self.tier)]).dec();
        }
        self.controller.dispatch(&mut inner);
    }
}
pub struct AdmissionPermit { controller: Option<Arc<AdmissionController>>, tier: usize, started: Instant }
impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        if let Some(controller) = &self.controller {
            let mut inner = controller.inner.lock().unwrap();
            inner.active[self.tier] -= 1;
            METRICS.active.with_label_values(&[label(self.tier)]).dec();
            let elapsed = self.started.elapsed().as_secs_f64();
            inner.latency_ewma = if inner.last_completion.elapsed() >= Duration::from_secs(5) { elapsed }
                else { 0.8 * inner.latency_ewma + 0.2 * elapsed };
            inner.last_completion = Instant::now();
            controller.dispatch(&mut inner);
        }
    }
}

#[derive(Clone)]
pub struct AdmissionRoute { pub controller: Arc<AdmissionController>, pub priority: Priority }
/// Hold the permit through handler completion. For WS/SSE this covers handshake
/// work; stream lifetime is bounded separately by the streaming registry.
pub async fn admission_middleware<B: Send + 'static>(State(route): State<AdmissionRoute>, request: Request<B>, next: Next<B>) -> Response {
    match route.controller.acquire(route.priority).await {
        Ok(_permit) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn controller() -> Arc<AdmissionController> {
        AdmissionController::new(AdmissionConfig { max_in_flight: 3, mutation_reserved: 1,
            shed_saturation: 1.0, ..AdmissionConfig::default() }).unwrap()
    }
    #[tokio::test]
    async fn mutations_have_reserved_capacity_and_first_queue_service() {
        let c = controller();
        let q1 = c.acquire(Priority::Quote).await.unwrap();
        let q2 = c.acquire(Priority::Quote).await.unwrap();
        let mut quote = Box::pin(c.acquire(Priority::Quote));
        assert!(futures::poll!(&mut quote).is_pending());
        let m1 = c.acquire(Priority::Mutation).await.unwrap();
        let mut mutation = Box::pin(c.acquire(Priority::Mutation));
        assert!(futures::poll!(&mut mutation).is_pending());
        assert!(c.acquire(Priority::Analytics).await.is_err());
        drop(q1);
        let m2 = mutation.await.unwrap();
        assert!(futures::poll!(&mut quote).is_pending());
        drop(q2);
        let q3 = quote.await.unwrap();
        drop((m1, m2, q3));
        assert_eq!(c.inner.lock().unwrap().active, [0; 3]);
    }
    #[tokio::test]
    async fn cancellation_releases_queued_and_granted_waiters() {
        let c = controller();
        let q1 = c.acquire(Priority::Quote).await.unwrap();
        let q2 = c.acquire(Priority::Quote).await.unwrap();
        let mut waiting = Box::pin(c.acquire(Priority::Quote));
        assert!(futures::poll!(&mut waiting).is_pending());
        drop(waiting);
        assert!(c.inner.lock().unwrap().queues[1].is_empty());
        let mut granted = Box::pin(c.acquire(Priority::Quote));
        assert!(futures::poll!(&mut granted).is_pending());
        drop(q1); // dispatch grants the queued request before it is polled
        drop(granted);
        assert_eq!(c.inner.lock().unwrap().active, [0, 1, 0]);
        drop(q2);
        assert_eq!(c.inner.lock().unwrap().active, [0; 3]);
    }
    #[tokio::test]
    async fn quotes_are_dispatched_before_waiting_analytics() {
        let c = controller();
        let q1 = c.acquire(Priority::Quote).await.unwrap();
        let q2 = c.acquire(Priority::Quote).await.unwrap();
        let mut analytics = Box::pin(c.acquire(Priority::Analytics));
        assert!(futures::poll!(&mut analytics).is_pending());
        let mut quote = Box::pin(c.acquire(Priority::Quote));
        assert!(futures::poll!(&mut quote).is_pending());
        drop(q1);
        let q3 = quote.await.unwrap();
        assert!(futures::poll!(&mut analytics).is_pending());
        drop(q2);
        let a1 = analytics.await.unwrap();
        drop((q3, a1));
    }
    #[tokio::test]
    async fn middleware_returns_problem_details_and_retry_after() {
        use axum::{body::Body, routing::get, Router};
        use tower::ServiceExt;
        let c = controller();
        let _active = [c.acquire(Priority::Mutation).await.unwrap(),
            c.acquire(Priority::Mutation).await.unwrap(), c.acquire(Priority::Mutation).await.unwrap()];
        let app = Router::new().route("/quotes", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(AdmissionRoute {
                controller: c, priority: Priority::Quote,
            }, admission_middleware));
        let response = app.oneshot(Request::builder().uri("/quotes").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["content-type"], "application/problem+json");
        assert!(response.headers().contains_key("retry-after"));
    }
    #[tokio::test]
    async fn queue_bounds_and_wait_deadlines_apply_to_all_tiers() {
        for tier in [Priority::Mutation, Priority::Quote, Priority::Analytics] {
            let c = AdmissionController::new(AdmissionConfig { max_in_flight: 2, mutation_reserved: 1,
                queue_depth: [1; 3], wait_timeout: [Duration::from_millis(5); 3],
                shed_saturation: 1.0, ..AdmissionConfig::default() }).unwrap();
            let active = if tier == Priority::Mutation {
                vec![c.acquire(tier).await.unwrap(), c.acquire(tier).await.unwrap()]
            } else { vec![c.acquire(tier).await.unwrap()] };
            let mut pending = Box::pin(c.acquire(tier));
            assert!(futures::poll!(&mut pending).is_pending());
            assert!(c.acquire(tier).await.is_err());
            assert!(pending.await.is_err());
            assert!(c.inner.lock().unwrap().queues.iter().all(VecDeque::is_empty));
            drop(active);
            assert_eq!(c.inner.lock().unwrap().active, [0; 3]);
        }
    }
    #[tokio::test]
    async fn latency_sheds_reads_but_never_preempts_an_order_and_recovers() {
        let c = controller();
        let order = c.acquire(Priority::Mutation).await.unwrap();
        { let mut inner = c.inner.lock().unwrap(); inner.latency_ewma = 1.0; }
        assert!(c.acquire(Priority::Quote).await.is_err());
        assert!(c.acquire(Priority::Analytics).await.is_err());
        assert_eq!(c.inner.lock().unwrap().active[0], 1);
        let second = c.acquire(Priority::Mutation).await.unwrap();
        c.inner.lock().unwrap().last_completion = Instant::now() - Duration::from_secs(6);
        assert!(c.acquire(Priority::Quote).await.is_ok());
        drop((order, second));
    }
    #[tokio::test]
    async fn disabled_admission_bypasses_limits() {
        let c = AdmissionController::new(AdmissionConfig { enabled: false, ..AdmissionConfig::default() }).unwrap();
        let mut permits = Vec::new();
        for _ in 0..200 { permits.push(c.acquire(Priority::Analytics).await.unwrap()); }
        assert_eq!(c.inner.lock().unwrap().active, [0; 3]);
    }
    #[tokio::test]
    async fn burst_load_has_bounded_queue_and_completion_time() {
        let c = controller();
        let start = Instant::now();
        let mut jobs = Vec::new();
        for _ in 0..256 {
            let c = c.clone();
            jobs.push(tokio::spawn(async move {
                if let Ok(_permit) = c.acquire(Priority::Quote).await {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }));
        }
        for job in jobs { job.await.unwrap(); }
        assert!(start.elapsed() < Duration::from_secs(2));
        let inner = c.inner.lock().unwrap();
        assert_eq!(inner.active, [0; 3]);
        assert!(inner.queues.iter().all(VecDeque::is_empty));
    }
    #[test]
    fn invalid_config_is_rejected() {
        assert!(AdmissionConfig { max_in_flight: 0, ..AdmissionConfig::default() }.validate().is_err());
        assert!(AdmissionConfig { shed_saturation: f64::NAN, ..AdmissionConfig::default() }.validate().is_err());
    }
}
