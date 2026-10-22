//! Operational kill switch and per-key circuit breakers for swap order intake.
//!
//! [`SwapGate`] is the single entry point a quote/order handler calls before
//! accepting new work:
//!
//! 1. The global kill switch rejects all new orders when disabled. It fails
//!    closed: the gate only admits work while the switch is explicitly on.
//! 2. A tenant breaker and a provider breaker each reject new work while open.
//!    Breakers trip on a rolling-window failure rate (errors plus calls slower
//!    than `slow_call_threshold`), stay open for `open_duration`, then admit a
//!    single half-open probe whose outcome closes or re-opens the breaker.
//!
//! Work already accepted is never affected: callers hold an [`Admission`] and
//! must report its outcome through [`Admission::complete`] or drop it via
//! [`Admission::release`] so the breaker bookkeeping stays correct.
//!
//! State is process-local. Multi-instance sharing (Redis) is not implemented
//! here; each instance trips independently.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Thresholds shared by every breaker in a [`SwapGate`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BreakerConfig {
    /// Length of the rolling window over which outcomes are counted.
    pub window: Duration,
    /// Minimum samples in the window before the failure rate is evaluated.
    pub min_calls: usize,
    /// Failure fraction (0.0..=1.0) at or above which the breaker trips.
    pub failure_rate_threshold: f64,
    /// Calls slower than this count as failures.
    pub slow_call_threshold: Duration,
    /// How long an open breaker rejects work before allowing a probe.
    pub open_duration: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(60),
            min_calls: 20,
            failure_rate_threshold: 0.5,
            slow_call_threshold: Duration::from_secs(2),
            open_duration: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerState {
    pub fn as_str(self) -> &'static str {
        match self {
            BreakerState::Closed => "closed",
            BreakerState::Open => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }
}

/// Proof that a call was admitted. `probe` marks the single half-open probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permit {
    probe: bool,
}

#[derive(Debug)]
struct BreakerInner {
    state: BreakerState,
    opened_at: Option<Instant>,
    probe_in_flight: bool,
    /// `(completed_at, failed)` for calls completed while closed.
    samples: VecDeque<(Instant, bool)>,
}

/// A single breaker for one key (a tenant or a liquidity provider).
#[derive(Debug)]
pub struct CircuitBreaker {
    key: String,
    config: BreakerConfig,
    inner: Mutex<BreakerInner>,
}

impl CircuitBreaker {
    pub fn new(key: impl Into<String>, config: BreakerConfig) -> Self {
        Self {
            key: key.into(),
            config,
            inner: Mutex::new(BreakerInner {
                state: BreakerState::Closed,
                opened_at: None,
                probe_in_flight: false,
                samples: VecDeque::new(),
            }),
        }
    }

    pub fn state(&self) -> BreakerState {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).state
    }

    /// Admit a call at `now`, or return the state that rejected it.
    pub fn try_acquire(&self, now: Instant) -> Result<Permit, BreakerState> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.state == BreakerState::Open {
            let elapsed = inner
                .opened_at
                .map_or(Duration::MAX, |opened| now.duration_since(opened));
            if elapsed < self.config.open_duration {
                return Err(BreakerState::Open);
            }
            self.transition(&mut inner, BreakerState::HalfOpen, now);
        }
        match inner.state {
            BreakerState::Closed => Ok(Permit { probe: false }),
            BreakerState::HalfOpen if !inner.probe_in_flight => {
                inner.probe_in_flight = true;
                Ok(Permit { probe: true })
            }
            state => Err(state),
        }
    }

    /// Record the outcome of an admitted call.
    pub fn record(&self, permit: Permit, now: Instant, latency: Duration, success: bool) {
        let failed = !success || latency > self.config.slow_call_threshold;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if permit.probe {
            inner.probe_in_flight = false;
            if inner.state == BreakerState::HalfOpen {
                let next = if failed {
                    BreakerState::Open
                } else {
                    BreakerState::Closed
                };
                self.transition(&mut inner, next, now);
            }
            return;
        }
        // Results from calls admitted before a trip are stale; only closed-state
        // outcomes feed the rolling window.
        if inner.state != BreakerState::Closed {
            return;
        }
        inner.samples.push_back((now, failed));
        self.prune(&mut inner, now);
        if inner.samples.len() >= self.config.min_calls
            && Self::failure_rate(&inner.samples) >= self.config.failure_rate_threshold
        {
            self.transition(&mut inner, BreakerState::Open, now);
        }
    }

    /// Drop an admitted call without an outcome, e.g. when a later check in the
    /// same admission rejected it. Frees a half-open probe slot if held.
    pub fn release(&self, permit: Permit) {
        if permit.probe {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.probe_in_flight = false;
        }
    }

    fn prune(&self, inner: &mut BreakerInner, now: Instant) {
        while let Some(&(at, _)) = inner.samples.front() {
            if now.duration_since(at) <= self.config.window {
                break;
            }
            inner.samples.pop_front();
        }
    }

    fn failure_rate(samples: &VecDeque<(Instant, bool)>) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let failures = samples.iter().filter(|(_, failed)| *failed).count();
        failures as f64 / samples.len() as f64
    }

    fn transition(&self, inner: &mut BreakerInner, next: BreakerState, now: Instant) {
        if inner.state == next {
            return;
        }
        let previous = inner.state;
        inner.state = next;
        inner.samples.clear();
        inner.probe_in_flight = false;
        inner.opened_at = (next == BreakerState::Open).then_some(now);
        if next == BreakerState::Open {
            tracing::warn!(
                key = %self.key,
                from = previous.as_str(),
                to = next.as_str(),
                "swap circuit breaker tripped"
            );
        } else {
            tracing::info!(
                key = %self.key,
                from = previous.as_str(),
                to = next.as_str(),
                "swap circuit breaker state changed"
            );
        }
        crate::metrics::swap_breaker_transitions_total().add(
            1,
            &[opentelemetry::KeyValue::new("state", next.as_str())],
        );
    }
}

/// Holds breakers keyed by tenant or provider id, created on first use.
#[derive(Debug)]
pub struct BreakerRegistry {
    config: BreakerConfig,
    breakers: Mutex<HashMap<String, Arc<CircuitBreaker>>>,
}

impl BreakerRegistry {
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            breakers: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &str) -> Arc<CircuitBreaker> {
        let mut breakers = self.breakers.lock().unwrap_or_else(|e| e.into_inner());
        breakers
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(CircuitBreaker::new(key, self.config)))
            .clone()
    }
}

/// Why a new swap order was refused at the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapRejection {
    /// The global kill switch is off.
    KillSwitchOff,
    /// The tenant's breaker is open or its half-open probe is taken.
    TenantBreakerOpen,
    /// The provider's breaker is open or its half-open probe is taken.
    ProviderBreakerOpen,
}

impl std::fmt::Display for SwapRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapRejection::KillSwitchOff => write!(f, "swap order intake is disabled"),
            SwapRejection::TenantBreakerOpen => write!(f, "tenant swap circuit breaker is open"),
            SwapRejection::ProviderBreakerOpen => {
                write!(f, "liquidity provider circuit breaker is open")
            }
        }
    }
}

impl std::error::Error for SwapRejection {}

/// Admitted work for one order. Report the outcome with [`Admission::complete`].
#[derive(Debug)]
pub struct Admission {
    tenant: (Arc<CircuitBreaker>, Permit),
    provider: (Arc<CircuitBreaker>, Permit),
}

impl Admission {
    /// Record the order's outcome against both breakers.
    pub fn complete(self, latency: Duration, success: bool) {
        let now = Instant::now();
        let (tenant, tenant_permit) = self.tenant;
        let (provider, provider_permit) = self.provider;
        tenant.record(tenant_permit, now, latency, success);
        provider.record(provider_permit, now, latency, success);
    }
}

/// Global kill switch plus per-tenant and per-provider breakers.
#[derive(Debug)]
pub struct SwapGate {
    accepting: AtomicBool,
    tenants: BreakerRegistry,
    providers: BreakerRegistry,
}

impl SwapGate {
    pub fn new(accepting: bool, config: BreakerConfig) -> Self {
        Self {
            accepting: AtomicBool::new(accepting),
            tenants: BreakerRegistry::new(config),
            providers: BreakerRegistry::new(config),
        }
    }

    pub fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Flip the global kill switch. `actor` is recorded in the audit log line.
    pub fn set_accepting(&self, accepting: bool, actor: &str) {
        let previous = self.accepting.swap(accepting, Ordering::SeqCst);
        if previous != accepting {
            tracing::warn!(
                actor,
                accepting,
                previous,
                audit = true,
                "swap order intake kill switch toggled"
            );
        }
    }

    /// Gate a new order from `tenant_id` routed to `provider`. Checks the kill
    /// switch first, then the provider, then the tenant. If the tenant check
    /// rejects, a provider probe slot taken earlier in this call is released.
    pub fn admit(&self, tenant_id: &str, provider: &str) -> Result<Admission, SwapRejection> {
        if !self.is_accepting() {
            return Err(SwapRejection::KillSwitchOff);
        }
        let now = Instant::now();

        let provider_breaker = self.providers.get(provider);
        let provider_permit = provider_breaker
            .try_acquire(now)
            .map_err(|_| SwapRejection::ProviderBreakerOpen)?;

        let tenant_breaker = self.tenants.get(tenant_id);
        let tenant_permit = match tenant_breaker.try_acquire(now) {
            Ok(permit) => permit,
            Err(_) => {
                provider_breaker.release(provider_permit);
                return Err(SwapRejection::TenantBreakerOpen);
            }
        };

        Ok(Admission {
            tenant: (tenant_breaker, tenant_permit),
            provider: (provider_breaker, provider_permit),
        })
    }
}

static SWAP_GATE: OnceLock<SwapGate> = OnceLock::new();

/// Install the process-wide gate from config. Later calls are ignored.
pub fn init_swap_gate(accepting: bool, config: BreakerConfig) {
    let _ = SWAP_GATE.set(SwapGate::new(accepting, config));
}

/// The process-wide gate. Falls back to accepting with default thresholds if
/// [`init_swap_gate`] has not run (e.g. in tests or the CLI).
pub fn swap_gate() -> &'static SwapGate {
    SWAP_GATE.get_or_init(|| SwapGate::new(true, BreakerConfig::default()))
}
