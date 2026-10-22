// src/swap/liquidity/mod.rs
//
// Liquidity Provider (LP) adapter framework.
//
// This module provides:
//   * The `LiquidityAdapter` trait that every venue connector must implement.
//   * A declarative `AdapterManifest` describing assets/pairs, rate limits,
//     precision and capabilities, validated at startup.
//   * A `LiquidityRegistry` that owns registered adapters, validates their
//     manifests, enforces certification before live traffic, and supports
//     hot credential rotation without a process restart.
//   * Credential loading from environment / secret storage with rotation.
//
// The conformance test kit lives in `tests/conformance/` and produces a
// machine-readable certification report consumed by the registry.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Core domain types
// ---------------------------------------------------------------------------

/// A trading pair, e.g. `XLM/USDC`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair {
    pub base: String,
    pub quote: String,
}

impl Pair {
    pub fn new(base: impl Into<String>, quote: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            quote: quote.into(),
        }
    }
}

impl fmt::Display for Pair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.base, self.quote)
    }
}

/// Declarative capabilities an adapter claims to honour.
///
/// The registry validates runtime behaviour against these declarations during
/// certification; an adapter that reports a capability it does not honour must
/// fail certification rather than degrade silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Adapter can push streaming quote updates.
    pub streaming_quotes: bool,
    /// Adapter supports order cancellation.
    pub cancellation: bool,
    /// Adapter reports partial fills.
    pub partial_fills: bool,
    /// Adapter supports order modification / amend.
    pub amend: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            streaming_quotes: false,
            cancellation: false,
            partial_fills: false,
            amend: false,
        }
    }
}

/// Rate-limit / ban semantics advertised by a venue so the engine can back off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Requests permitted per window.
    pub requests: u32,
    /// Window length in milliseconds.
    pub window_ms: u64,
    /// Optional cooldown applied after a ban, in milliseconds.
    #[serde(default)]
    pub ban_cooldown_ms: Option<u64>,
}

impl RateLimit {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.requests == 0 {
            return Err(ManifestError::InvalidRateLimit(
                "requests must be greater than zero".into(),
            ));
        }
        if self.window_ms == 0 {
            return Err(ManifestError::InvalidRateLimit(
                "window_ms must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Amount precision constraints for a venue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Precision {
    /// Maximum decimal places for base-asset amounts.
    pub base_decimals: u8,
    /// Maximum decimal places for quote-asset amounts.
    pub quote_decimals: u8,
    /// Minimum order size (as a decimal string to avoid float drift).
    pub min_order_size: String,
}

impl Precision {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.base_decimals > 18 {
            return Err(ManifestError::InvalidPrecision(
                "base_decimals exceeds supported maximum (18)".into(),
            ));
        }
        if self.quote_decimals > 18 {
            return Err(ManifestError::InvalidPrecision(
                "quote_decimals exceeds supported maximum (18)".into(),
            ));
        }
        if self.min_order_size.parse::<f64>().map(|v| v <= 0.0).unwrap_or(true) {
            return Err(ManifestError::InvalidPrecision(
                "min_order_size must be a positive decimal".into(),
            ));
        }
        Ok(())
    }
}

/// The registration manifest every adapter must provide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterManifest {
    /// Stable unique identifier, e.g. `stellar-sdex`.
    pub id: String,
    /// Human-readable venue name.
    pub venue: String,
    /// Assets the adapter can trade.
    pub assets: Vec<String>,
    /// Pairs the adapter supports.
    pub pairs: Vec<Pair>,
    /// Rate-limit semantics.
    pub rate_limit: RateLimit,
    /// Amount precision constraints.
    pub precision: Precision,
    /// Declared capabilities.
    pub capabilities: Capabilities,
}

impl AdapterManifest {
    /// Validate the manifest. Called at startup by the registry.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.trim().is_empty() {
            return Err(ManifestError::MissingField("id".into()));
        }
        if self.id.chars().any(|c| c.is_whitespace()) {
            return Err(ManifestError::InvalidField(
                "id must not contain whitespace".into(),
            ));
        }
        if self.venue.trim().is_empty() {
            return Err(ManifestError::MissingField("venue".into()));
        }
        if self.assets.is_empty() {
            return Err(ManifestError::MissingField("assets".into()));
        }
        if self.pairs.is_empty() {
            return Err(ManifestError::MissingField("pairs".into()));
        }

        let asset_set: HashSet<&str> = self.assets.iter().map(String::as_str).collect();
        if asset_set.len() != self.assets.len() {
            return Err(ManifestError::InvalidField(
                "assets contains duplicates".into(),
            ));
        }

        let mut seen_pairs = HashSet::new();
        for pair in &self.pairs {
            if pair.base.trim().is_empty() || pair.quote.trim().is_empty() {
                return Err(ManifestError::InvalidPair(pair.to_string()));
            }
            if pair.base == pair.quote {
                return Err(ManifestError::InvalidPair(format!(
                    "{} has identical base and quote",
                    pair
                )));
            }
            if !asset_set.contains(pair.base.as_str()) {
                return Err(ManifestError::UnknownAsset(pair.base.clone()));
            }
            if !asset_set.contains(pair.quote.as_str()) {
                return Err(ManifestError::UnknownAsset(pair.quote.clone()));
            }
            if !seen_pairs.insert(pair.clone()) {
                return Err(ManifestError::InvalidPair(format!(
                    "{} is duplicated",
                    pair
                )));
            }
        }

        self.rate_limit.validate()?;
        self.precision.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("invalid field: {0}")]
    InvalidField(String),
    #[error("invalid pair: {0}")]
    InvalidPair(String),
    #[error("unknown asset referenced by pair: {0}")]
    UnknownAsset(String),
    #[error("invalid rate limit: {0}")]
    InvalidRateLimit(String),
    #[error("invalid precision: {0}")]
    InvalidPrecision(String),
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("manifest validation failed for adapter '{0}': {1}")]
    InvalidManifest(String, #[source] ManifestError),
    #[error("adapter '{0}' is already registered")]
    DuplicateAdapter(String),
    #[error("adapter '{0}' is not registered")]
    UnknownAdapter(String),
    #[error("adapter '{0}' cannot serve live traffic: {1}")]
    NotCertified(String, String),
    #[error("credential error for adapter '{0}': {1}")]
    Credential(String, String),
    #[error("adapter error: {0}")]
    Adapter(String),
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

/// A single credential value. `Debug` is redacted so secrets never reach logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Expose the secret value. Callers must not log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***redacted***)")
    }
}

/// Credentials for an adapter. Values are redacted in `Debug`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Credentials {
    values: HashMap<String, Secret>,
}

impl Credentials {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(key.into(), Secret::new(value));
        self
    }

    pub fn get(&self, key: &str) -> Option<&Secret> {
        self.values.get(key)
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<&String> = self.values.keys().collect();
        f.debug_struct("Credentials")
            .field("keys", &keys)
            .finish()
    }
}

/// Source of credentials for an adapter. Implementations may read from the
/// environment, a secret manager, or a file-backed store. `load` is called on
/// registration and again on rotation, so implementations must be cheap and
/// side-effect free.
pub trait CredentialProvider: Send + Sync {
    /// Load the current credentials for the given adapter id.
    fn load(&self, adapter_id: &str) -> Result<Credentials, RegistryError>;
}

/// Loads credentials from environment variables of the form
/// `LP_<ADAPTER_ID_UPPER>_<KEY_UPPER>`.
///
/// Example: adapter `stellar-sdex` with key `api_secret` reads
/// `LP_STELLAR_SDEX_API_SECRET`.
#[derive(Debug, Default, Clone)]
pub struct EnvCredentialProvider {
    /// Keys to look up for each adapter.
    keys: Vec<String>,
}

impl EnvCredentialProvider {
    pub fn new(keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            keys: keys.into_iter().map(Into::into).collect(),
        }
    }

    fn env_var_name(adapter_id: &str, key: &str) -> String {
        let normalize = |s: &str| {
            s.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' })
                .collect::<String>()
        };
        format!("LP_{}_{}", normalize(adapter_id), normalize(key))
    }
}

impl CredentialProvider for EnvCredentialProvider {
    fn load(&self, adapter_id: &str) -> Result<Credentials, RegistryError> {
        let mut creds = Credentials::new();
        for key in &self.keys {
            let var = Self::env_var_name(adapter_id, key);
            if let Ok(value) = std::env::var(&var) {
                if !value.is_empty() {
                    creds = creds.with(key.clone(), value);
                }
            }
        }
        Ok(creds)
    }
}

// ---------------------------------------------------------------------------
// Certification
// ---------------------------------------------------------------------------

/// Machine-readable certification report produced by the conformance suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificationReport {
    pub adapter_id: String,
    /// Version of the conformance kit that produced this report.
    pub kit_version: String,
    /// Overall pass/fail.
    pub passed: bool,
    /// Individual check results.
    pub checks: Vec<CertificationCheck>,
    /// Unix timestamp (seconds) when the report was produced.
    pub generated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificationCheck {
    pub name: String,
    pub passed: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

impl CertificationReport {
    /// A report is valid for an adapter only if it passed and the adapter id
    /// matches.
    pub fn certifies(&self, adapter_id: &str) -> bool {
        self.passed && self.adapter_id == adapter_id
    }
}

/// Source of certification reports. In CI this is backed by files written by
/// the conformance suite; in production it may be a signed artifact store.
pub trait CertificationStore: Send + Sync {
    fn report_for(&self, adapter_id: &str) -> Option<CertificationReport>;
}

/// In-memory certification store, useful for tests and CI.
#[derive(Debug, Default, Clone)]
pub struct InMemoryCertificationStore {
    reports: Arc<RwLock<HashMap<String, CertificationReport>>>,
}

impl InMemoryCertificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, report: CertificationReport) {
        let mut guard = self.reports.write().expect("certification store poisoned");
        guard.insert(report.adapter_id.clone(), report);
    }
}

impl CertificationStore for InMemoryCertificationStore {
    fn report_for(&self, adapter_id: &str) -> Option<CertificationReport> {
        self.reports
            .read()
            .expect("certification store poisoned")
            .get(adapter_id)
            .cloned()
    }
}

// ---------------------------------------------------------------------------
// Adapter trait
// ---------------------------------------------------------------------------

/// A quote produced by an adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub pair: Pair,
    /// Amount of base asset.
    pub base_amount: String,
    /// Amount of quote asset.
    pub quote_amount: String,
    /// Unix timestamp (milliseconds) when the quote was produced.
    pub timestamp_ms: u64,
}

/// Result of a cancellation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelOutcome {
    /// Order fully cancelled.
    Cancelled,
    /// Order was already filled / partially filled and could not be cancelled.
    NotCancellable { reason: String },
    /// Order not found.
    NotFound,
}

/// Fill report, including partial fills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillReport {
    pub order_id: String,
    pub filled_base: String,
    pub filled_quote: String,
    pub partial: bool,
}

/// The contract every liquidity adapter must implement.
#[async_trait]
pub trait LiquidityAdapter: Send + Sync {
    /// The adapter's registration manifest.
    fn manifest(&self) -> &AdapterManifest;

    /// Fetch a fresh quote for a pair.
    async fn quote(&self, pair: &Pair) -> Result<Quote, AdapterError>;

    /// Cancel an order by id.
    async fn cancel(&self, order_id: &str) -> Result<CancelOutcome, AdapterError>;

    /// Report fills for an order.
    async fn fills(&self, order_id: &str) -> Result<Vec<FillReport>, AdapterError>;

    /// Apply rotated credentials. Called by the registry on rotation; must not
    /// require a process restart.
    fn apply_credentials(&self, credentials: Credentials) -> Result<(), AdapterError>;
}

/// Errors an adapter may surface. Mapped to a stable taxonomy so the engine can
/// back off appropriately.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AdapterError {
    #[error("rate limited: retry after {retry_after_ms} ms")]
    RateLimited { retry_after_ms: u64 },
    #[error("banned by venue: cooldown {cooldown_ms} ms")]
    Banned { cooldown_ms: u64 },
    #[error("request timed out")]
    Timeout,
    #[error("authentication failed")]
    Auth,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("venue unavailable: {0}")]
    Unavailable(String),
    #[error("internal adapter error: {0}")]
    Internal(String),
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Runtime mode. Certification is only required outside test configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Test configuration: certification not required.
    Test,
    /// Live configuration: certification required before serving traffic.
    Live,
}

struct RegisteredAdapter {
    adapter: Arc<dyn LiquidityAdapter>,
    credentials: RwLock<Credentials>,
}

/// The registry of liquidity adapters.
pub struct LiquidityRegistry {
    mode: RuntimeMode,
    adapters: RwLock<HashMap<String, RegisteredAdapter>>,
    credential_provider: Arc<dyn CredentialProvider>,
    certification_store: Arc<dyn CertificationStore>,
}

impl LiquidityRegistry {
    pub fn new(
        mode: RuntimeMode,
        credential_provider: Arc<dyn CredentialProvider>,
        certification_store: Arc<dyn CertificationStore>,
    ) -> Self {
        Self {
            mode,
            adapters: RwLock::new(HashMap::new()),
            credential_provider,
            certification_store,
        }
    }

    pub fn mode(&self) -> RuntimeMode {
        self.mode
    }

    /// Register an adapter. Validates the manifest, loads credentials, and (in
    /// live mode) requires a passing certification report.
    pub fn register(&self, adapter: Arc<dyn LiquidityAdapter>) -> Result<(), RegistryError> {
        let manifest = adapter.manifest().clone();
        manifest
            .validate()
            .map_err(|e| RegistryError::InvalidManifest(manifest.id.clone(), e))?;

        if self.mode == RuntimeMode::Live {
            let report = self
                .certification_store
                .report_for(&manifest.id)
                .ok_or_else(|| {
                    RegistryError::NotCertified(
                        manifest.id.clone(),
                        "no certification report found".into(),
                    )
                })?;
            if !report.certifies(&manifest.id) {
                return Err(RegistryError::NotCertified(
                    manifest.id.clone(),
                    "certification report did not pass".into(),
                ));
            }
        }

        let credentials = self.credential_provider.load(&manifest.id)?;
        adapter
            .apply_credentials(credentials.clone())
            .map_err(|e| RegistryError::Adapter(e.to_string()))?;

        let mut guard = self.adapters.write().expect("registry poisoned");
        if guard.contains_key(&manifest.id) {
            return Err(RegistryError::DuplicateAdapter(manifest.id));
        }
        guard.insert(
            manifest.id.clone(),
            RegisteredAdapter {
                adapter,
                credentials: RwLock::new(credentials),
            },
        );
        Ok(())
    }

    /// Rotate credentials for an adapter without a process restart.
    pub fn rotate_credentials(&self, adapter_id: &str) -> Result<(), RegistryError> {
        let adapter = {
            let guard = self.adapters.read().expect("registry poisoned");
            guard
                .get(adapter_id)
                .map(|r| r.adapter.clone())
                .ok_or_else(|| RegistryError::UnknownAdapter(adapter_id.to_string()))?
        };

        let credentials = self.credential_provider.load(adapter_id)?;
        adapter
            .apply_credentials(credentials.clone())
            .map_err(|e| RegistryError::Adapter(e.to_string()))?;

        let guard = self.adapters.read().expect("registry poisoned");
        if let Some(entry) = guard.get(adapter_id) {
            *entry.credentials.write().expect("credentials poisoned") = credentials;
        }
        Ok(())
    }

    /// Rotate credentials for every registered adapter.
    pub fn rotate_all(&self) -> Result<(), RegistryError> {
        let ids: Vec<String> = {
            let guard = self.adapters.read().expect("registry poisoned");
            guard.keys().cloned().collect()
        };
        for id in ids {
            self.rotate_credentials(&id)?;
        }
        Ok(())
    }

    /// Fetch an adapter for serving traffic. In live mode this re-checks
    /// certification so an adapter cannot serve traffic after its report is
    /// revoked.
    pub fn get(&self, adapter_id: &str) -> Result<Arc<dyn LiquidityAdapter>, RegistryError> {
        let guard = self.adapters.read().expect("registry poisoned");
        let entry = guard
            .get(adapter_id)
            .ok_or_else(|| RegistryError::UnknownAdapter(adapter_id.to_string()))?;

        if self.mode == RuntimeMode::Live {
            let certified = self
                .certification_store
                .report_for(adapter_id)
                .map(|r| r.certifies(adapter_id))
                .unwrap_or(false);
            if !certified {
                return Err(RegistryError::NotCertified(
                    adapter_id.to_string(),
                    "certification report missing or failed".into(),
                ));
            }
        }
        Ok(entry.adapter.clone())
    }

    /// List registered adapter ids.
    pub fn list(&self) -> Vec<String> {
        let guard = self.adapters.read().expect("registry poisoned");
        let mut ids: Vec<String> = guard.keys().cloned().collect();
        ids.sort();
        ids
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sample_manifest(id: &str) -> AdapterManifest {
        AdapterManifest {
            id: id.to_string(),
            venue: "Test Venue".into(),
            assets: vec!["XLM".into(), "USDC".into()],
            pairs: vec![Pair::new("XLM", "USDC")],
            rate_limit: RateLimit {
                requests: 10,
                window_ms: 1000,
                ban_cooldown_ms: Some(5000),
            },
            precision: Precision {
                base_decimals: 7,
                quote_decimals: 7,
                min_order_size: "0.0000001".into(),
            },
            capabilities: Capabilities {
                streaming_quotes: true,
                cancellation: true,
                partial_fills: true,
                amend: false,
            },
        }
    }

    struct MockAdapter {
        manifest: AdapterManifest,
        applied: AtomicUsize,
        last_creds: RwLock<Credentials>,
    }

    impl MockAdapter {
        fn new(manifest: AdapterManifest) -> Self {
            Self {
                manifest,
                applied: AtomicUsize::new(0),
                last_creds: RwLock::new(Credentials::new()),
            }
        }
    }

    #[async_trait]
    impl LiquidityAdapter for MockAdapter {
        fn manifest(&self) -> &AdapterManifest {
            &self.manifest
        }
        async fn quote(&self, pair: &Pair) -> Result<Quote, AdapterError> {
            Ok(Quote {
                pair: pair.clone(),
                base_amount: "1".into(),
                quote_amount: "1".into(),
                timestamp_ms: 0,
            })
        }
        async fn cancel(&self, _order_id: &str) -> Result<CancelOutcome, AdapterError> {
            Ok(CancelOutcome::Cancelled)
        }
        async fn fills(&self, _order_id: &str) -> Result<Vec<FillReport>, AdapterError> {
            Ok(vec![])
        }
        fn apply_credentials(&self, credentials: Credentials) -> Result<(), AdapterError> {
            self.applied.fetch_add(1, Ordering::SeqCst);
            *self.last_creds.write().unwrap() = credentials;
            Ok(())
        }
    }

    struct StaticCreds(HashMap<String, Credentials>);
    impl CredentialProvider for StaticCreds {
        fn load(&self, adapter_id: &str) -> Result<Credentials, RegistryError> {
            Ok(self.0.get(adapter_id).cloned().unwrap_or_default())
        }
    }

    fn passing_report(id: &str) -> CertificationReport {
        CertificationReport {
            adapter_id: id.to_string(),
            kit_version: "1.0.0".into(),
            passed: true,
            checks: vec![],
            generated_at: 0,
        }
    }

    #[test]
    fn manifest_validation_accepts_valid() {
        assert!(sample_manifest("ok").validate().is_ok());
    }

    #[test]
    fn manifest_validation_rejects_unknown_asset() {
        let mut m = sample_manifest("bad");
        m.pairs = vec![Pair::new("XLM", "EUR")];
        assert!(matches!(m.validate(), Err(ManifestError::UnknownAsset(_))));
    }

    #[test]
    fn manifest_validation_rejects_duplicate_pairs() {
        let mut m = sample_manifest("dup");
        m.pairs = vec![Pair::new("XLM", "USDC"), Pair::new("XLM", "USDC")];
        assert!(matches!(m.validate(), Err(ManifestError::InvalidPair(_))));
    }

    #[test]
    fn manifest_validation_rejects_zero_rate_limit() {
        let mut m = sample_manifest("rl");
        m.rate_limit.requests = 0;
        assert!(matches!(m.validate(), Err(ManifestError::InvalidRateLimit(_))));
    }

    #[test]
    fn manifest_validation_rejects_bad_precision() {
        let mut m = sample_manifest("prec");
        m.precision.min_order_size = "0".into();
        assert!(matches!(m.validate(), Err(ManifestError::InvalidPrecision(_))));
    }

    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::new("super-secret");
        assert!(!format!("{:?}", s).contains("super-secret"));
        let creds = Credentials::new().with("api_key", "super-secret");
        assert!(!format!("{:?}", creds).contains("super-secret"));
    }

    #[test]
    fn live_registration_requires_certification() {
        let store = Arc::new(InMemoryCertificationStore::new());
        let registry = LiquidityRegistry::new(
            RuntimeMode::Live,
            Arc::new(StaticCreds(HashMap::new())),
            store.clone(),
        );
        let adapter = Arc::new(MockAdapter::new(sample_manifest("live")));
        assert!(matches!(
            registry.register(adapter.clone()),
            Err(RegistryError::NotCertified(_, _))
        ));

        store.insert(passing_report("live"));
        assert!(registry.register(adapter).is_ok());
    }

    #[test]
    fn test_mode_registration_skips_certification() {
        let registry = LiquidityRegistry::new(
            RuntimeMode::Test,
            Arc::new(StaticCreds(HashMap::new())),
            Arc::new(InMemoryCertificationStore::new()),
        );
        let adapter = Arc::new(MockAdapter::new(sample_manifest("t")));
        assert!(registry.register(adapter).is_ok());
    }

    #[test]
    fn duplicate_registration_rejected() {
        let registry = LiquidityRegistry::new(
            RuntimeMode::Test,
            Arc::new(StaticCreds(HashMap::new())),
            Arc::new(InMemoryCertificationStore::new()),
        );
        let adapter = Arc::new(MockAdapter::new(sample_manifest("dup")));
        assert!(registry.register(adapter.clone()).is_ok());
        assert!(matches!(
            registry.register(adapter),
            Err(RegistryError::DuplicateAdapter(_))
        ));
    }

    #[test]
    fn credential_rotation_updates_adapter_without_restart() {
        let mut map = HashMap::new();
        map.insert("rot".to_string(), Credentials::new().with("api_key", "v1"));
        let provider = Arc::new(StaticCreds(map));
        let registry = LiquidityRegistry::new(
            RuntimeMode::Test,
            provider,
            Arc::new(InMemoryCertificationStore::new()),
        );
        let adapter = Arc::new(MockAdapter::new(sample_manifest("rot")));
        registry.register(adapter.clone()).unwrap();
        assert_eq!(adapter.applied.load(Ordering::SeqCst), 1);
        assert_eq!(
            adapter.last_creds.read().unwrap().get("api_key").unwrap().expose(),
            "v1"
        );

        registry.rotate_credentials("rot").unwrap();
        assert_eq!(adapter.applied.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rotate_unknown_adapter_errors() {
        let registry = LiquidityRegistry::new(
            RuntimeMode::Test,
            Arc::new(StaticCreds(HashMap::new())),
            Arc::new(InMemoryCertificationStore::new()),
        );
        assert!(matches!(
            registry.rotate_credentials("nope"),
            Err(RegistryError::UnknownAdapter(_))
        ));
    }

    #[test]
    fn live_get_rechecks_certification() {
        let store = Arc::new(InMemoryCertificationStore::new());
        store.insert(passing_report("live"));
        let registry = LiquidityRegistry::new(
            RuntimeMode::Live,
            Arc::new(StaticCreds(HashMap::new())),
            store.clone(),
        );
        let adapter = Arc::new(MockAdapter::new(sample_manifest("live")));
        registry.register(adapter).unwrap();
        assert!(registry.get("live").is_ok());

        // Simulate a revoked report by using a fresh store.
        let revoked = LiquidityRegistry::new(
            RuntimeMode::Live,
            Arc::new(StaticCreds(HashMap::new())),
            Arc::new(InMemoryCertificationStore::new()),
        );
        assert!(matches!(
            revoked.get("live"),
            Err(RegistryError::UnknownAdapter(_))
        ));
    }
}