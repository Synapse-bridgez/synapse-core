//! Conformance test kit for liquidity provider (LP) adapters.
//!
//! This module is intentionally independent of production wiring so it can be
//! run against a mock adapter in CI and against a live sandbox adapter manually.
//!
//! The kit exercises a candidate adapter against the documented contract:
//! - quote freshness
//! - amount precision
//! - error mapping
//! - timeout behavior
//! - cancellation semantics
//! - partial-fill reporting
//!
//! It also validates that an adapter's declared capabilities match its runtime
//! behavior, and emits a machine-readable certification report per run.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Core domain types (mirrors the adapter contract in `src/swap/liquidity/`)
// ---------------------------------------------------------------------------

/// A trading pair identifier, e.g. `"XLM/USDC"`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pair(pub String);

impl Pair {
    pub fn new(base: &str, quote: &str) -> Self {
        Pair(format!("{}/{}", base, quote))
    }
}

impl fmt::Display for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A fixed-precision decimal amount represented as an integer scaled by
/// `10^scale`. Adapters must report amounts at the declared precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amount {
    /// Scaled integer value.
    pub value: i128,
    /// Number of decimal places (scale) this amount is expressed in.
    pub scale: u8,
}

impl Amount {
    pub fn new(value: i128, scale: u8) -> Self {
        Amount { value, scale }
    }

    /// Convert to a different scale, returning `None` if precision would be lost.
    pub fn rescale(&self, target_scale: u8) -> Option<Amount> {
        if target_scale == self.scale {
            return Some(*self);
        }
        if target_scale > self.scale {
            let factor = 10i128.checked_pow((target_scale - self.scale) as u32)?;
            Some(Amount::new(self.value.checked_mul(factor)?, target_scale))
        } else {
            let factor = 10i128.checked_pow((self.scale - target_scale) as u32)?;
            if self.value % factor != 0 {
                None
            } else {
                Some(Amount::new(self.value / factor, target_scale))
            }
        }
    }
}

/// Declared capabilities of an adapter. The registry validates runtime
/// behavior against this declaration; adapters that report capabilities they
/// do not honour must fail certification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub streaming_quotes: bool,
    pub cancellation: bool,
    pub partial_fills: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            streaming_quotes: false,
            cancellation: false,
            partial_fills: false,
        }
    }
}

/// Rate-limit / ban semantics as reported by a venue so the engine can back
/// off appropriately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Maximum requests permitted per window.
    pub limit: u32,
    /// Window duration.
    pub window: Duration,
    /// If set, the venue has banned the client until this instant.
    pub banned_until: Option<Instant>,
}

/// Adapter registration manifest, validated at startup.
#[derive(Debug, Clone)]
pub struct AdapterManifest {
    /// Stable adapter identifier, e.g. `"stellar-sdex"`.
    pub id: String,
    /// Supported trading pairs.
    pub pairs: Vec<Pair>,
    /// Supported assets (symbols).
    pub assets: Vec<String>,
    /// Declared amount precision (decimal places).
    pub precision: u8,
    /// Declared capabilities.
    pub capabilities: Capabilities,
    /// Declared rate limits.
    pub rate_limit: RateLimitInfo,
}

/// Errors surfaced by an adapter. The engine maps these to its own error
/// taxonomy; the conformance kit verifies the mapping is exhaustive and
/// consistent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    /// The venue rejected the request due to rate limiting.
    RateLimited { retry_after: Option<Duration> },
    /// The venue banned the client.
    Banned { until: Option<Instant> },
    /// The request timed out.
    Timeout,
    /// The order was cancelled.
    Cancelled,
    /// The venue returned an invalid or unsupported request.
    InvalidRequest(String),
    /// A transient transport/venue error.
    Transient(String),
    /// Any other error.
    Other(String),
}

/// A quote returned by an adapter.
#[derive(Debug, Clone)]
pub struct Quote {
    pub pair: Pair,
    /// Price expressed at the adapter's declared precision.
    pub price: Amount,
    /// Available amount at the declared precision.
    pub amount: Amount,
    /// When the quote was produced.
    pub timestamp: Instant,
}

/// A fill report returned by an adapter.
#[derive(Debug, Clone)]
pub struct FillReport {
    pub pair: Pair,
    /// Amount filled at the declared precision.
    pub filled: Amount,
    /// Amount requested at the declared precision.
    pub requested: Amount,
    /// Whether the fill is partial.
    pub partial: bool,
}

/// The trait every LP adapter implements. The conformance kit drives this
/// trait directly.
pub trait LpAdapter: Send + Sync {
    /// Return the adapter's registration manifest.
    fn manifest(&self) -> &AdapterManifest;

    /// Fetch a fresh quote for `pair`.
    fn quote(&self, pair: &Pair) -> Result<Quote, AdapterError>;

    /// Submit an order and return the resulting fill report.
    fn submit(&self, pair: &Pair, amount: Amount) -> Result<FillReport, AdapterError>;

    /// Cancel an in-flight order identified by `order_id`.
    fn cancel(&self, order_id: u64) -> Result<(), AdapterError>;

    /// Return the current rate-limit / ban state.
    fn rate_limit_state(&self) -> RateLimitInfo;
}

// ---------------------------------------------------------------------------
// Manifest validation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    EmptyId,
    NoPairs,
    NoAssets,
    ZeroPrecision,
    ExcessivePrecision(u8),
    DuplicatePair(Pair),
    DuplicateAsset(String),
    ZeroRateLimit,
    ZeroRateWindow,
    PairAssetMismatch { pair: Pair, asset: String },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::EmptyId => write!(f, "adapter id must not be empty"),
            ManifestError::NoPairs => write!(f, "adapter must declare at least one pair"),
            ManifestError::NoAssets => write!(f, "adapter must declare at least one asset"),
            ManifestError::ZeroPrecision => write!(f, "precision must be greater than zero"),
            ManifestError::ExcessivePrecision(p) => {
                write!(f, "precision {} exceeds maximum of 18", p)
            }
            ManifestError::DuplicatePair(p) => write!(f, "duplicate pair: {}", p),
            ManifestError::DuplicateAsset(a) => write!(f, "duplicate asset: {}", a),
            ManifestError::ZeroRateLimit => write!(f, "rate limit must be greater than zero"),
            ManifestError::ZeroRateWindow => write!(f, "rate window must be greater than zero"),
            ManifestError::PairAssetMismatch { pair, asset } => {
                write!(f, "pair {} references undeclared asset {}", pair, asset)
            }
        }
    }
}

impl std::error::Error for ManifestError {}

/// Maximum supported precision (decimal places).
pub const MAX_PRECISION: u8 = 18;

/// Validate an adapter manifest. Returns all discovered problems.
pub fn validate_manifest(manifest: &AdapterManifest) -> Result<(), Vec<ManifestError>> {
    let mut errors = Vec::new();

    if manifest.id.trim().is_empty() {
        errors.push(ManifestError::EmptyId);
    }
    if manifest.pairs.is_empty() {
        errors.push(ManifestError::NoPairs);
    }
    if manifest.assets.is_empty() {
        errors.push(ManifestError::NoAssets);
    }
    if manifest.precision == 0 {
        errors.push(ManifestError::ZeroPrecision);
    } else if manifest.precision > MAX_PRECISION {
        errors.push(ManifestError::ExcessivePrecision(manifest.precision));
    }
    if manifest.rate_limit.limit == 0 {
        errors.push(ManifestError::ZeroRateLimit);
    }
    if manifest.rate_limit.window.is_zero() {
        errors.push(ManifestError::ZeroRateWindow);
    }

    // Duplicate pairs.
    let mut seen_pairs = std::collections::BTreeSet::new();
    for pair in &manifest.pairs {
        if !seen_pairs.insert(pair.clone()) {
            errors.push(ManifestError::DuplicatePair(pair.clone()));
        }
    }

    // Duplicate assets.
    let mut seen_assets = std::collections::BTreeSet::new();
    for asset in &manifest.assets {
        if !seen_assets.insert(asset.clone()) {
            errors.push(ManifestError::DuplicateAsset(asset.clone()));
        }
    }

    // Every pair must reference declared assets.
    for pair in &manifest.pairs {
        let parts: Vec<&str> = pair.0.split('/').collect();
        if parts.len() != 2 {
            errors.push(ManifestError::PairAssetMismatch {
                pair: pair.clone(),
                asset: pair.0.clone(),
            });
            continue;
        }
        for part in parts {
            if !manifest.assets.iter().any(|a| a == part) {
                errors.push(ManifestError::PairAssetMismatch {
                    pair: pair.clone(),
                    asset: part.to_string(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// Credential loading & rotation
// ---------------------------------------------------------------------------

/// A source of credentials. Implementations may read from the environment or
/// a secret store. Secrets must never be logged or committed.
pub trait CredentialSource: Send + Sync {
    /// Fetch the current credential value for `key`, if present.
    fn get(&self, key: &str) -> Option<String>;
}

/// In-memory credential source used by tests and as a fallback.
#[derive(Debug, Default, Clone)]
pub struct MemoryCredentialSource {
    inner: Arc<Mutex<BTreeMap<String, String>>>,
}

impl MemoryCredentialSource {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: impl Into<String>, value: impl Into<String>) {
        self.inner
            .lock()
            .expect("credential store poisoned")
            .insert(key.into(), value.into());
    }

    pub fn remove(&self, key: &str) {
        self.inner
            .lock()
            .expect("credential store poisoned")
            .remove(key);
    }
}

impl CredentialSource for MemoryCredentialSource {
    fn get(&self, key: &str) -> Option<String> {
        self.inner
            .lock()
            .expect("credential store poisoned")
            .get(key)
            .cloned()
    }
}

/// Environment-backed credential source. Reads `LP_<ADAPTER>_<KEY>` style
/// variables. Never logs values.
#[derive(Debug, Clone)]
pub struct EnvCredentialSource {
    prefix: String,
}

impl EnvCredentialSource {
    pub fn new(prefix: impl Into<String>) -> Self {
        EnvCredentialSource {
            prefix: prefix.into(),
        }
    }

    fn var_name(&self, key: &str) -> String {
        format!(
            "{}_{}",
            self.prefix.to_uppercase().replace('-', "_"),
            key.to_uppercase().replace('-', "_")
        )
    }
}

impl CredentialSource for EnvCredentialSource {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(self.var_name(key)).ok()
    }
}

/// Holds the currently active credential and supports rotation without a
/// process restart. The active value is swapped atomically.
#[derive(Debug, Clone)]
pub struct RotatingCredential {
    key: String,
    source: Arc<dyn CredentialSource>,
    current: Arc<Mutex<Option<String>>>,
    generation: Arc<AtomicU64>,
}

impl RotatingCredential {
    /// Create a new rotating credential, loading the initial value from
    /// `source`.
    pub fn new(key: impl Into<String>, source: Arc<dyn CredentialSource>) -> Self {
        let key = key.into();
        let initial = source.get(&key);
        RotatingCredential {
            key,
            source,
            current: Arc::new(Mutex::new(initial)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The credential key (not the secret value).
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The current generation counter. Incremented on each successful rotation.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Whether a credential value is currently loaded.
    pub fn is_loaded(&self) -> bool {
        self.current
            .lock()
            .expect("credential poisoned")
            .is_some()
    }

    /// Rotate the credential by re-reading from the source. Returns `true` if
    /// a value was loaded (possibly unchanged), `false` if the source had no
    /// value. Does not require a process restart.
    pub fn rotate(&self) -> bool {
        let new_value = self.source.get(&self.key);
        let mut guard = self.current.lock().expect("credential poisoned");
        let changed = *guard != new_value;
        *guard = new_value;
        drop(guard);
        if changed {
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        self.is_loaded()
    }

    /// Run `f` with the current credential value, if loaded. The value is
    /// never exposed outside this closure and must not be logged.
    pub fn with_value<R>(&self, f: impl FnOnce(&str) -> R) -> Option<R> {
        let guard = self.current.lock().expect("credential poisoned");
        guard.as_deref().map(f)
    }
}

// ---------------------------------------------------------------------------
// Certification report
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckStatus {
    Passed,
    Failed(String),
    Skipped(String),
}

impl CheckStatus {
    pub fn is_passed(&self) -> bool {
        matches!(self, CheckStatus::Passed)
    }
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub duration: Duration,
}

#[derive(Debug, Clone)]
pub struct CertificationReport {
    pub adapter_id: String,
    pub started_at_unix: u64,
    pub duration: Duration,
    pub checks: Vec<CheckResult>,
    pub certified: bool,
}

impl CertificationReport {
    /// Serialize the report to a machine-readable JSON string.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str(&format!("\"adapter_id\":{},", json_string(&self.adapter_id)));
        out.push_str(&format!("\"started_at_unix\":{},", self.started_at_unix));
        out.push_str(&format!(
            "\"duration_ms\":{},",
            self.duration.as_millis()
        ));
        out.push_str(&format!("\"certified\":{},", self.certified));
        out.push_str("\"checks\":[");
        for (i, check) in self.checks.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str(&format!("\"name\":{},", json_string(&check.name)));
            out.push_str(&format!(
                "\"duration_ms\":{},",
                check.duration.as_millis()
            ));
            match &check.status {
                CheckStatus::Passed => {
                    out.push_str("\"status\":\"passed\"");
                }
                CheckStatus::Failed(reason) => {
                    out.push_str("\"status\":\"failed\",");
                    out.push_str(&format!("\"reason\":{}", json_string(reason)));
                }
                CheckStatus::Skipped(reason) => {
                    out.push_str("\"status\":\"skipped\",");
                    out.push_str(&format!("\"reason\":{}", json_string(reason)));
                }
            }
            out.push('}');
        }
        out.push_str("]}");
        out
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Conformance suite
// ---------------------------------------------------------------------------

/// Configuration for a conformance run.
#[derive(Debug, Clone)]
pub struct ConformanceConfig {
    /// Maximum acceptable age of a quote before it is considered stale.
    pub max_quote_age: Duration,
    /// Maximum acceptable latency for a single adapter call.
    pub call_timeout: Duration,
    /// Number of quote samples to take when checking freshness.
    pub quote_samples: usize,
}

impl Default for ConformanceConfig {
    fn default() -> Self {
        ConformanceConfig {
            max_quote_age: Duration::from_secs(5),
            call_timeout: Duration::from_secs(2),
            quote_samples: 3,
        }
    }
}

/// The conformance suite. Drives an adapter through the documented contract
/// and produces a certification report.
pub struct ConformanceSuite {
    config: ConformanceConfig,
}

impl ConformanceSuite {
    pub fn new(config: ConformanceConfig) -> Self {
        ConformanceSuite { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(ConformanceConfig::default())
    }

    /// Run the full suite against `adapter` and return a certification report.
    pub fn run(&self, adapter: &dyn LpAdapter) -> CertificationReport {
        let started_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let start = Instant::now();

        let mut checks = Vec::new();

        checks.push(self.check_manifest(adapter));
        checks.push(self.check_quote_freshness(adapter));
        checks.push(self.check_amount_precision(adapter));
        checks.push(self.check_error_mapping(adapter));
        checks.push(self.check_timeout_behavior(adapter));
        checks.push(self.check_cancellation_semantics(adapter));
        checks.push(self.check_partial_fill_reporting(adapter));
        checks.push(self.check_capability_honesty(adapter));

        let certified = checks.iter().all(|c| c.status.is_passed());

        CertificationReport {
            adapter_id: adapter.manifest().id.clone(),
            started_at_unix,
            duration: start.elapsed(),
            checks,
            certified,
        }
    }

    fn timed(&self, name: &str, f: impl FnOnce() -> CheckStatus) -> CheckResult {
        let start = Instant::now();
        let status = f();
        CheckResult {
            name: name.to_string(),
            status,
            duration: start.elapsed(),
        }
    }

    fn check_manifest(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("manifest_validation", || {
            match validate_manifest(adapter.manifest()) {
                Ok(()) => CheckStatus::Passed,
                Err(errors) => CheckStatus::Failed(format!(
                    "manifest invalid: {}",
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                )),
            }
        })
    }

    fn check_quote_freshness(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("quote_freshness", || {
            let manifest = adapter.manifest();
            let pair = match manifest.pairs.first() {
                Some(p) => p.clone(),
                None => return CheckStatus::Failed("no pairs declared".into()),
            };
            for _ in 0..self.config.quote_samples.max(1) {
                let before = Instant::now();
                match adapter.quote(&pair) {
                    Ok(quote) => {
                        let age = before.elapsed();
                        if age > self.config.max_quote_age {
                            return CheckStatus::Failed(format!(
                                "quote for {} took {:?}, exceeds max age {:?}",
                                pair, age, self.config.max_quote_age
                            ));
                        }
                        if quote.timestamp > Instant::now() {
                            return CheckStatus::Failed(
                                "quote timestamp is in the future".into(),
                            );
                        }
                        if quote.pair != pair {
                            return CheckStatus::Failed(format!(
                                "quote returned pair {} but requested {}",
                                quote.pair, pair
                            ));
                        }
                    }
                    Err(e) => {
                        return CheckStatus::Failed(format!(
                            "quote for {} failed: {:?}",
                            pair, e
                        ));
                    }
                }
            }
            CheckStatus::Passed
        })
    }

    fn check_amount_precision(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("amount_precision", || {
            let manifest = adapter.manifest();
            let pair = match manifest.pairs.first() {
                Some(p) => p.clone(),
                None => return CheckStatus::Failed("no pairs declared".into()),
            };
            let precision = manifest.precision;
            match adapter.quote(&pair) {
                Ok(quote) => {
                    if quote.price.scale != precision {
                        return CheckStatus::Failed(format!(
                            "quote price scale {} != declared precision {}",
                            quote.price.scale, precision
                        ));
                    }
                    if quote.amount.scale != precision {
                        return CheckStatus::Failed(format!(
                            "quote amount scale {} != declared precision {}",
                            quote.amount.scale, precision
                        ));
                    }
                }
                Err(e) => {
                    return CheckStatus::Failed(format!("quote failed: {:?}", e));
                }
            }
            let requested = Amount::new(1_000_000, precision);
            match adapter.submit(&pair, requested) {
                Ok(fill) => {
                    if fill.filled.scale != precision {
                        return CheckStatus::Failed(format!(
                            "fill scale {} != declared precision {}",
                            fill.filled.scale, precision
                        ));
                    }
                    if fill.requested.scale != precision {
                        return CheckStatus::Failed(format!(
                            "fill requested scale {} != declared precision {}",
                            fill.requested.scale, precision
                        ));
                    }
                    if fill.filled.value > fill.requested.value {
                        return CheckStatus::Failed(
                            "filled amount exceeds requested amount".into(),
                        );
                    }
                }
                Err(e) => {
                    return CheckStatus::Failed(format!("submit failed: {:?}", e));
                }
            }
            CheckStatus::Passed
        })
    }

    fn check_error_mapping(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("error_mapping", || {
            let manifest = adapter.manifest();
            let pair = match manifest.pairs.first() {
                Some(p) => p.clone(),
                None => return CheckStatus::Failed("no pairs declared".into()),
            };
            // Submitting a zero amount must be rejected with a mapped error,
            // not a panic or an unmapped variant.
            let zero = Amount::new(0, manifest.precision);
            match adapter.submit(&pair, zero) {
                Ok(_) => CheckStatus::Failed(
                    "zero-amount submit was accepted; expected InvalidRequest".into(),
                ),
                Err(AdapterError::InvalidRequest(_)) => CheckStatus::Passed,
                Err(other) => CheckStatus::Failed(format!(
                    "zero-amount submit mapped to unexpected error: {:?}",
                    other
                )),
            }
        })
    }

    fn check_timeout_behavior(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("timeout_behavior", || {
            let manifest = adapter.manifest();
            let pair = match manifest.pairs.first() {
                Some(p) => p.clone(),
                None => return CheckStatus::Failed("no pairs declared".into()),
            };
            // A quote must complete within the configured call timeout.
            let start = Instant::now();
            let result = adapter.quote(&pair);
            let elapsed = start.elapsed();
            if elapsed > self.config.call_timeout {
                return CheckStatus::Failed(format!(
                    "quote took {:?}, exceeds call timeout {:?}",
                    elapsed, self.config.call_timeout
                ));
            }
            match result {
                Ok(_) => CheckStatus::Passed,
                Err(AdapterError::Timeout) => CheckStatus::Passed,
                Err(e) => CheckStatus::Failed(format!(
                    "quote returned unexpected error: {:?}",
                    e
                )),
            }
        })
    }

    fn check_cancellation_semantics(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("cancellation_semantics", || {
            let caps = &adapter.manifest().capabilities;
            if !caps.cancellation {
                return CheckStatus::Skipped(
                    "adapter does not declare cancellation capability".into(),
                );
            }
            // Cancelling an unknown order must map to a defined error, not panic.
            match adapter.cancel(u64::MAX) {
                Ok(()) => CheckStatus::Failed(
                    "cancelling unknown order succeeded; expected an error".into(),
                ),
                Err(AdapterError::Cancelled) | Err(AdapterError::InvalidRequest(_)) => {
                    CheckStatus::Passed
                }
                Err(e) => CheckStatus::Failed(format!(
                    "cancel mapped to unexpected error: {:?}",
                    e
                )),
            }
        })
    }

    fn check_partial_fill_reporting(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("partial_fill_reporting", || {
            let manifest = adapter.manifest();
            let caps = &manifest.capabilities;
            if !caps.partial_fills {
                return CheckStatus::Skipped(
                    "adapter does not declare partial-fill capability".into(),
                );
            }
            let pair = match manifest.pairs.first() {
                Some(p) => p.clone(),
                None => return CheckStatus::Failed("no pairs declared".into()),
            };
            let requested = Amount::new(1_000_000, manifest.precision);
            match adapter.submit(&pair, requested) {
                Ok(fill) => {
                    let expected_partial = fill.filled.value < fill.requested.value;
                    if fill.partial != expected_partial {
                        return CheckStatus::Failed(format!(
                            "partial flag {} inconsistent with filled {} / requested {}",
                            fill.partial, fill.filled.value, fill.requested.value
                        ));
                    }
                    CheckStatus::Passed
                }
                Err(e) => CheckStatus::Failed(format!("submit failed: {:?}", e)),
            }
        })
    }

    fn check_capability_honesty(&self, adapter: &dyn LpAdapter) -> CheckResult {
        self.timed("capability_honesty", || {
            let caps = &adapter.manifest().capabilities;
            // Streaming quotes: if declared, the adapter must return quotes
            // without error across repeated samples.
            if caps.streaming_quotes {
                let pair = match adapter.manifest().pairs.first() {
                    Some(p) => p.clone(),
                    None => return CheckStatus::Failed("no pairs declared".into()),
                };
                for _ in 0..3 {
                    if let Err(e) = adapter.quote(&pair) {
                        return CheckStatus::Failed(format!(
                            "declared streaming_quotes but quote failed: {:?}",
                            e
                        ));
                    }
                }
            }
            // Cancellation: if declared, cancel must be callable.
            if caps.cancellation {
                if let Err(AdapterError::InvalidRequest(_)) = adapter.cancel(0) {
                    // InvalidRequest is acceptable for unknown order.
                }
            }
            // Rate-limit state must be internally consistent.
            let rl = adapter.rate_limit_state();
            if rl.limit == 0 {
                return CheckStatus::Failed("rate limit state reports zero limit".into());
            }
            if rl.window.is_zero() {
                return CheckStatus::Failed("rate limit state reports zero window".into());
            }
            CheckStatus::Passed
        })
    }
}

// ---------------------------------------------------------------------------
// Mock adapter (conformant)
// ---------------------------------------------------------------------------

/// A conformant mock adapter used in CI. Honours all declared capabilities.
pub struct MockAdapter {
    manifest: AdapterManifest,
    precision: u8,
    rate_limit: RateLimitInfo,
}

impl MockAdapter {
    pub fn new() -> Self {
        let precision = 7u8;
        let assets = vec!["XLM".to_string(), "USDC".to_string()];
        let pairs = vec![Pair::new("XLM", "USDC")];
        let manifest = AdapterManifest {
            id: "mock-conformant".to_string(),
            pairs,
            assets,
            precision,
            capabilities: Capabilities {
                streaming_quotes: true,
                cancellation: true,
                partial_fills: true,
            },
            rate_limit: RateLimitInfo {
                limit: 100,
                window: Duration::from_secs(1),
                banned_until: None,
            },
        };
        MockAdapter {
            manifest,
            precision,
            rate_limit: RateLimitInfo {
                limit: 100,
                window: Duration::from_secs(1),
                banned_until: None,
            },
        }
    }
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LpAdapter for MockAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn quote(&self, pair: &Pair) -> Result<Quote, AdapterError> {
        if !self.manifest.pairs.contains(pair) {
            return Err(AdapterError::InvalidRequest(format!(
                "unsupported pair {}",
                pair
            )));
        }
        Ok(Quote {
            pair: pair.clone(),
            price: Amount::new(1_000_000, self.precision),
            amount: Amount::new(10_000_000, self.precision),
            timestamp: Instant::now(),
        })
    }

    fn submit(&self, pair: &Pair, amount: Amount) -> Result<FillReport, AdapterError> {
        if !self.manifest.pairs.contains(pair) {
            return Err(AdapterError::InvalidRequest(format!(
                "unsupported pair {}",
                pair
            )));
        }
        if amount.value <= 0 {
            return Err(AdapterError::InvalidRequest(
                "amount must be positive".into(),
            ));
        }
        // Simulate a partial fill: fill half of the requested amount.
        let filled = Amount::new(amount.value / 2, amount.scale);
        Ok(FillReport {
            pair: pair.clone(),
            filled,
            requested: amount,
            partial: filled.value < amount.value,
        })
    }

    fn cancel(&self, order_id: u64) -> Result<(), AdapterError> {
        if order_id == u64::MAX {
            return Err(AdapterError::InvalidRequest("unknown order".into()));
        }
        Ok(())
    }

    fn rate_limit_state(&self) -> RateLimitInfo {
        self.rate_limit.clone()
    }
}

// ---------------------------------------------------------------------------
// Deliberately non-conformant adapter
// ---------------------------------------------------------------------------

/// A deliberately non-conformant adapter used to prove the suite fails it.
/// It declares capabilities it does not honour, reports wrong precision, and
/// accepts zero-amount submits.
pub struct NonConformantAdapter {
    manifest: AdapterManifest,
}

impl NonConformantAdapter {
    pub fn new() -> Self {
        let assets = vec!["XLM".to_string(), "USDC".to_string()];
        let pairs = vec![Pair::new("XLM", "USDC")];
        let manifest = AdapterManifest {
            id: "mock-non-conformant".to_string(),
            pairs,
            assets,
            precision: 7,
            capabilities: Capabilities {
                streaming_quotes: true,
                cancellation: true,
                partial_fills: true,
            },
            rate_limit: RateLimitInfo {
                limit: 100,
                window: Duration::from_secs(1),
                banned_until: None,
            },
        };
        NonConformantAdapter { manifest }
    }
}

impl Default for NonConformantAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl LpAdapter for NonConformantAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    fn quote(&self, pair: &Pair) -> Result<Quote, AdapterError> {
        // Wrong precision: reports scale 2 instead of declared 7.
        Ok(Quote {
            pair: pair.clone(),
            price: Amount::new(100, 2),
            amount: Amount::new(1000, 2),
            timestamp: Instant::now(),
        })
    }

    fn submit(&self, pair: &Pair, amount: Amount) -> Result<FillReport, AdapterError> {
        // Accepts zero amounts (should be rejected).
        Ok(FillReport {
            pair: pair.clone(),
            filled: amount,
            requested: amount,
            partial: false,
        })
    }

    fn cancel(&self, _order_id: u64) -> Result<(), AdapterError> {
        // Declares cancellation but always succeeds, even for unknown orders.
        Ok(())
    }

    fn rate_limit_state(&self) -> RateLimitInfo {
        RateLimitInfo {
            limit: 0,
            window: Duration::ZERO,
            banned_until: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> AdapterManifest {
        AdapterManifest {
            id: "test".to_string(),
            pairs: vec![Pair::new("XLM", "USDC")],
            assets: vec!["XLM".to_string(), "USDC".to_string()],
            precision: 7,
            capabilities: Capabilities::default(),
            rate_limit: RateLimitInfo {
                limit: 10,
                window: Duration::from_secs(1),
                banned_until: None,
            },
        }
    }

    #[test]
    fn valid_manifest_passes() {
        assert!(validate_manifest(&sample_manifest()).is_ok());
    }

    #[test]
    fn empty_id_fails() {
        let mut m = sample_manifest();
        m.id = "  ".to_string();
        let errs = validate_manifest(&m).unwrap_err();
        assert!(errs.contains(&ManifestError::EmptyId));
    }

    #[test]
    fn no_pairs_fails() {
        let mut m = sample_manifest();
        m.pairs.clear();
        let errs = validate_manifest(&m).unwrap_err();
        assert!(errs.contains(&ManifestError::NoPairs));
    }

    #[test]
    fn no_assets_fails() {
        let mut m = sample_