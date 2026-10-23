use crate::secrets::SecretsManager;
use anyhow::Result;
use dotenvy::dotenv;
use ipnet::IpNet;
use std::collections::HashMap;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Active environment profile
#[derive(Debug, Clone, PartialEq)]
pub enum AppEnv {
    Development,
    Staging,
    Production,
}

/// Endpoint class for query timeout enforcement
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointClass {
    /// Interactive API reads and writes (default, shortest timeout)
    Interactive,
    /// Admin/report operations (medium timeout)
    AdminReport,
    /// Background jobs and maintenance tasks (longest timeout)
    BackgroundJob,
}

impl AppEnv {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "production" | "prod" => AppEnv::Production,
            "staging" => AppEnv::Staging,
            _ => AppEnv::Development,
        }
    }
}

impl AppEnv {
    pub fn as_str(&self) -> &'static str {
        match self {
            AppEnv::Development => "development",
            AppEnv::Staging => "staging",
            AppEnv::Production => "production",
        }
    }
}

/// Load the profile-specific .env file (.env.development, .env.staging, .env.production)
/// then fall back to the base .env. Profile file is loaded first so base .env can override.
fn load_env_profile(app_env: &AppEnv) {
    // Load base .env first (lowest priority)
    dotenv().ok();
    // Load profile-specific file (higher priority — values set here override base .env)
    let profile_file = format!(".env.{}", app_env.as_str());
    dotenvy::from_filename(&profile_file).ok();
}

/// Apply profile defaults for any env vars not already set
fn apply_profile_defaults(app_env: &AppEnv) {
    match app_env {
        AppEnv::Development => {
            // Verbose logging, relaxed limits, longer timeouts
            set_default("LOG_FORMAT", "text");
            set_default("RUST_LOG", "debug");
            set_default("DEFAULT_RATE_LIMIT", "10000");
            set_default("WHITELIST_RATE_LIMIT", "100000");
            set_default("DB_TIMEOUT_READ_SECS", "30");
            set_default("DB_TIMEOUT_WRITE_SECS", "60");
            set_default("DB_STATEMENT_TIMEOUT_MS", "60000");
        }
        AppEnv::Staging => {
            set_default("LOG_FORMAT", "json");
            set_default("RUST_LOG", "info");
            set_default("DEFAULT_RATE_LIMIT", "500");
            set_default("WHITELIST_RATE_LIMIT", "5000");
            set_default("DB_TIMEOUT_READ_SECS", "10");
            set_default("DB_TIMEOUT_WRITE_SECS", "20");
            set_default("DB_STATEMENT_TIMEOUT_MS", "30000");
        }
        AppEnv::Production => {
            // JSON logging, strict rate limits, short timeouts
            set_default("LOG_FORMAT", "json");
            set_default("RUST_LOG", "warn");
            set_default("DEFAULT_RATE_LIMIT", "100");
            set_default("WHITELIST_RATE_LIMIT", "1000");
            set_default("DB_TIMEOUT_READ_SECS", "5");
            set_default("DB_TIMEOUT_WRITE_SECS", "10");
            set_default("DB_STATEMENT_TIMEOUT_MS", "30000");
        }
    }
}

/// Set an env var only if it is not already set
fn set_default(key: &str, value: &str) {
    if env::var(key).is_err() {
        // SAFETY: single-threaded at config load time
        unsafe { env::set_var(key, value) }
    }
}

#[derive(Debug, Clone)]
pub enum AllowedIps {
    Any,
    Cidrs(Vec<IpNet>),
}

#[derive(Debug, Clone)]
pub enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Clone)]
pub struct DbTimeoutConfig {
    /// Timeout for read queries (SELECT), in seconds. Default: 5
    pub read_query_secs: u64,
    /// Timeout for write queries (INSERT/UPDATE/DELETE), in seconds. Default: 10
    pub write_query_secs: u64,
    /// Timeout for admin queries (migrations, maintenance), in seconds. Default: 60
    pub admin_query_secs: u64,
}

impl Default for DbTimeoutConfig {
    fn default() -> Self {
        Self {
            read_query_secs: 5,
            write_query_secs: 10,
            admin_query_secs: 60,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PerClassTimeoutConfig {
    /// Interactive endpoint timeout in milliseconds (default: 5000ms)
    pub interactive_timeout_ms: u64,
    /// Admin/report endpoint timeout in milliseconds (default: 30000ms)
    pub admin_report_timeout_ms: u64,
    /// Background job timeout in milliseconds (default: 60000ms)
    pub background_job_timeout_ms: u64,
}

impl Default for PerClassTimeoutConfig {
    fn default() -> Self {
        Self {
            interactive_timeout_ms: 5000,
            admin_report_timeout_ms: 30000,
            background_job_timeout_ms: 60000,
        }
    }
}

impl PerClassTimeoutConfig {
    /// Get timeout in milliseconds for the given endpoint class
    pub fn get_timeout_ms(&self, class: EndpointClass) -> u64 {
        match class {
            EndpointClass::Interactive => self.interactive_timeout_ms,
            EndpointClass::AdminReport => self.admin_report_timeout_ms,
            EndpointClass::BackgroundJob => self.background_job_timeout_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Swap engine runtime parameters (hot-reloadable, versioned, audited)
// ---------------------------------------------------------------------------

/// A single tunable swap-engine parameter. Values are stored as `f64` so a
/// single bounds-checked representation covers limits, spreads, timeouts and
/// feature flags (0.0 = disabled, 1.0 = enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapParam {
    /// Maximum order size in base units
    MaxOrderSize,
    /// Minimum order size in base units
    MinOrderSize,
    /// Bid/ask spread in basis points
    SpreadBps,
    /// Quote validity timeout in milliseconds
    QuoteTimeoutMs,
    /// Feature flag: enable a venue (0.0/1.0)
    VenueEnabled,
}

impl SwapParam {
    /// Documented inclusive bounds for each parameter. Validation rejects any
    /// value outside these ranges; the bounds are the contract for operators.
    pub fn bounds(&self) -> (f64, f64) {
        match self {
            SwapParam::MaxOrderSize => (0.0, 1_000_000_000.0),
            SwapParam::MinOrderSize => (0.0, 1_000_000_000.0),
            SwapParam::SpreadBps => (0.0, 10_000.0),
            SwapParam::QuoteTimeoutMs => (100.0, 600_000.0),
            SwapParam::VenueEnabled => (0.0, 1.0),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SwapParam::MaxOrderSize => "max_order_size",
            SwapParam::MinOrderSize => "min_order_size",
            SwapParam::SpreadBps => "spread_bps",
            SwapParam::QuoteTimeoutMs => "quote_timeout_ms",
            SwapParam::VenueEnabled => "venue_enabled",
        }
    }
}

/// A validated, immutable snapshot of swap parameters for a single tenant.
/// In-flight orders capture this at acceptance so mid-flight behavior does not
/// silently change when a new version is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapConfigSnapshot {
    pub version: u64,
    pub tenant: Option<String>,
    pub values: HashMap<SwapParam, f64>,
}

impl SwapConfigSnapshot {
    pub fn get(&self, param: SwapParam) -> Option<f64> {
        self.values.get(&param).copied()
    }
}

/// A single proposed parameter change.
#[derive(Debug, Clone)]
pub struct ParamChange {
    pub param: SwapParam,
    pub value: f64,
}

/// Validation error for a configuration submission.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigValidationError {
    pub param: SwapParam,
    pub value: f64,
    pub min: f64,
    pub max: f64,
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parameter {} value {} out of bounds [{}, {}]",
            self.param.as_str(),
            self.value,
            self.min,
            self.max
        )
    }
}

impl std::error::Error for ConfigValidationError {}

/// Validate a batch of changes atomically. Returns the first error without
/// applying anything; callers must not mutate state on `Err`.
pub fn validate_changes(changes: &[ParamChange]) -> Result<(), ConfigValidationError> {
    for change in changes {
        let (min, max) = change.param.bounds();
        if change.value.is_nan() || change.value < min || change.value > max {
            return Err(ConfigValidationError {
                param: change.param,
                value: change.value,
                min,
                max,
            });
        }
    }
    Ok(())
}

/// Audit record for an applied configuration change.
#[derive(Debug, Clone)]
pub struct ConfigAuditRecord {
    pub version: u64,
    pub actor: String,
    pub timestamp: std::time::SystemTime,
    pub reason: String,
    pub before: HashMap<SwapParam, f64>,
    pub after: HashMap<SwapParam, f64>,
}

/// A subscriber notified on every applied configuration change. Subscribers
/// are invoked synchronously on apply; no polling loop is required.
pub trait ConfigSubscriber: Send + Sync {
    fn on_change(&self, snapshot: &SwapConfigSnapshot);
}

/// Error returned when an optimistic-concurrency version check fails.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionConflict {
    pub expected: u64,
    pub actual: u64,
}

impl std::fmt::Display for VersionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "configuration version conflict: expected {}, actual {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for VersionConflict {}

/// Typed configuration provider for swap-engine parameters. Holds a single
/// immutable snapshot swapped atomically; readers always see a consistent
/// view. Layered resolution: environment defaults < persisted store <
/// per-tenant overrides.
#[derive(Clone)]
pub struct SwapConfigProvider {
    inner: Arc<SwapConfigInner>,
}

struct SwapConfigInner {
    /// Global (non-tenant) snapshot, atomically swapped.
    global: RwLock<Arc<SwapConfigSnapshot>>,
    /// Per-tenant overrides layered on top of the global snapshot.
    tenant_overrides: RwLock<HashMap<String, HashMap<SwapParam, f64>>>,
    /// Monotonic version counter for optimistic concurrency.
    version: AtomicU64,
    /// Applied-change audit log (append-only).
    audit: RwLock<Vec<ConfigAuditRecord>>,
    /// Version history for rollback.
    history: RwLock<Vec<Arc<SwapConfigSnapshot>>>,
    /// Subscribers notified on change.
    subscribers: RwLock<Vec<Arc<dyn ConfigSubscriber>>>,
}

impl SwapConfigProvider {
    /// Build a provider from environment defaults (bootstrap fallback).
    pub fn from_env() -> Self {
        let mut values = HashMap::new();
        values.insert(SwapParam::MaxOrderSize, 1_000_000.0);
        values.insert(SwapParam::MinOrderSize, 1.0);
        values.insert(SwapParam::SpreadBps, 10.0);
        values.insert(SwapParam::QuoteTimeoutMs, 5_000.0);
        values.insert(SwapParam::VenueEnabled, 1.0);
        let snapshot = SwapConfigSnapshot {
            version: 1,
            tenant: None,
            values,
        };
        Self {
            inner: Arc::new(SwapConfigInner {
                global: RwLock::new(Arc::new(snapshot.clone())),
                tenant_overrides: RwLock::new(HashMap::new()),
                version: AtomicU64::new(1),
                audit: RwLock::new(Vec::new()),
                history: RwLock::new(vec![Arc::new(snapshot)]),
                subscribers: RwLock::new(Vec::new()),
            }),
        }
    }

    /// Register a subscriber notified on every applied change.
    pub fn subscribe(&self, subscriber: Arc<dyn ConfigSubscriber>) {
        if let Ok(mut subs) = self.inner.subscribers.write() {
            subs.push(subscriber);
        }
    }

    /// Current global snapshot (consistent view).
    pub fn snapshot(&self) -> Arc<SwapConfigSnapshot> {
        self.inner
            .global
            .read()
            .map(|g| g.clone())
            .unwrap_or_else(|_| Arc::new(SwapConfigSnapshot {
                version: 0,
                tenant: None,
                values: HashMap::new(),
            }))
    }

    /// Resolve the effective snapshot for a tenant: global values overlaid
    /// with per-tenant overrides. Missing overrides fall back to global.
    pub fn snapshot_for_tenant(&self, tenant: &str) -> Arc<SwapConfigSnapshot> {
        let global = self.snapshot();
        let overrides = self
            .inner
            .tenant_overrides
            .read()
            .ok()
            .and_then(|m| m.get(tenant).cloned())
            .unwrap_or_default();
        if overrides.is_empty() {
            return global;
        }
        let mut values = global.values.clone();
        for (k, v) in overrides {
            values.insert(k, v);
        }
        Arc::new(SwapConfigSnapshot {
            version: global.version,
            tenant: Some(tenant.to_string()),
            values,
        })
    }

    /// Dry-run validation without applying anything.
    pub fn validate(&self, changes: &[ParamChange]) -> Result<(), ConfigValidationError> {
        validate_changes(changes)
    }

    /// Apply a batch of changes atomically. `expected_version` enforces
    /// optimistic concurrency; on mismatch nothing is applied. On success the
    /// snapshot is swapped atomically, an audit record is appended, the
    /// version is bumped, and subscribers are notified.
    pub fn apply(
        &self,
        expected_version: u64,
        changes: &[ParamChange],
        actor: &str,
        reason: &str,
    ) -> Result<u64, ApplyError> {
        // Validate first — nothing partially applied on invalid input.
        self.validate(changes).map_err(ApplyError::Invalid)?;

        let current = self.snapshot();
        if current.version != expected_version {
            return Err(ApplyError::Conflict(VersionConflict {
                expected: expected_version,
                actual: current.version,
            }));
        }

        let mut after = current.values.clone();
        for change in changes {
            after.insert(change.param, change.value);
        }

        let new_version = self.inner.version.fetch_add(1, Ordering::SeqCst) + 1;
        let new_snapshot = Arc::new(SwapConfigSnapshot {
            version: new_version,
            tenant: None,
            values: after.clone(),
        });

        // Swap atomically.
        if let Ok(mut g) = self.inner.global.write() {
            *g = new_snapshot.clone();
        }

        let record = ConfigAuditRecord {
            version: new_version,
            actor: actor.to_string(),
            timestamp: std::time::SystemTime::now(),
            reason: reason.to_string(),
            before: current.values.clone(),
            after,
        };
        if let Ok(mut audit) = self.inner.audit.write() {
            audit.push(record);
        }
        if let Ok(mut history) = self.inner.history.write() {
            history.push(new_snapshot.clone());
        }

        // Notify subscribers without polling.
        if let Ok(subs) = self.inner.subscribers.read() {
            for sub in subs.iter() {
                sub.on_change(&new_snapshot);
            }
        }

        Ok(new_version)
    }

    /// Set a per-tenant override (validated). Missing overrides fall back to
    /// the global snapshot at resolution time.
    pub fn set_tenant_override(
        &self,
        tenant: &str,
        changes: &[ParamChange],
    ) -> Result<(), ConfigValidationError> {
        self.validate(changes)?;
        if let Ok(mut overrides) = self.inner.tenant_overrides.write() {
            let entry = overrides.entry(tenant.to_string()).or_default();
            for change in changes {
                entry.insert(change.param, change.value);
            }
        }
        Ok(())
    }

    /// View the audit log of applied changes.
    pub fn audit_log(&self) -> Vec<ConfigAuditRecord> {
        self.inner
            .audit
            .read()
            .map(|a| a.clone())
            .unwrap_or_default()
    }

    /// View version history (snapshots).
    pub fn history(&self) -> Vec<Arc<SwapConfigSnapshot>> {
        self.inner
            .history
            .read()
            .map(|h| h.clone())
            .unwrap_or_default()
    }

    /// Roll back to a previous version by re-applying its values as a new
    /// version (history is append-only; rollback is itself audited).
    pub fn rollback(
        &self,
        target_version: u64,
        actor: &str,
        reason: &str,
    ) -> Result<u64, ApplyError> {
        let target = self
            .inner
            .history
            .read()
            .ok()
            .and_then(|h| h.iter().find(|s| s.version == target_version).cloned())
            .ok_or(ApplyError::UnknownVersion(target_version))?;

        let current = self.snapshot();
        let changes: Vec<ParamChange> = target
            .values
            .iter()
            .map(|(param, value)| ParamChange {
                param: *param,
                value: *value,
            })
            .collect();
        self.apply(current.version, &changes, actor, reason)
    }
}

/// Errors from applying a configuration change.
#[derive(Debug)]
pub enum ApplyError {
    Invalid(ConfigValidationError),
    Conflict(VersionConflict),
    UnknownVersion(u64),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Invalid(e) => write!(f, "invalid configuration: {}", e),
            ApplyError::Conflict(e) => write!(f, "{}", e),
            ApplyError::UnknownVersion(v) => write!(f, "unknown configuration version {}", v),
        }
    }
}

impl std::error::Error for ApplyError {}

#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: AppEnv,
    pub server_port: u16,
    pub database_url: String,
    pub database_replica_url: Option<String>,
    /// Connection string for `pg_dump`/`pg_restore` (BackupService). Postgres
    /// refuses to `pg_dump` a table with `FORCE ROW LEVEL SECURITY` (both
    /// `transactions` and, since this fix, `settlements`) unless the
    /// connecting role bypasses RLS — `pg_dump`'s COPY would otherwise
    /// silently produce a filtered, incomplete backup, so Postgres errors
    /// instead. `database_url` is deliberately a NOBYPASSRLS role (see
    /// scripts/db-init/01-create-app-role.sql), so backups need a separate,
    /// explicitly RLS-bypassing connection — standard practice for backup
    /// tooling to use different credentials than the request-serving app.
    /// Defaults to `database_url` when unset, which will fail loudly (not
    /// silently under-back-up) the first time a backup actually runs against
    /// a NOBYPASSRLS role, per the `pg_dump` behavior described above.
    pub backup_database_url: Option<String>,
    pub stellar_horizon_url: String,
    pub anchor_webhook_secret: String,
    pub redis_url: String,
    pub default_rate_limit: u32,
    pub whitelist_rate_limit: u32,
    pub whitelisted_ips: String,
    pub log_format: LogFormat,
    pub allowed_ips: AllowedIps,
    /// Number of trusted reverse proxies / load balancers in front of this
    /// deployment. Used by `IpFilterLayer` to index into a possibly-spoofed
    /// `X-Forwarded-For` chain from the right instead of trusting the
    /// client-supplied leftmost entry. Must match the real proxy topology:
    /// too low and a client can spoof an allowed IP; too high and the
    /// filter reads into attacker-controlled parts of the header.
    pub trusted_proxy_depth: usize,
    pub backup_dir: String,
    pub backup_encryption_key: Option<String>,
    pub db_timeouts: DbTimeoutConfig,
    pub otlp_endpoint: Option<String>,
    // CORS
    pub cors_allowed_origins: Vec<String>,
    // Back-pressure
    pub max_pending_queue: u64,
    // DB pool sizing
    pub db_min_connections: u32,
    pub db_max_connections: u32,
    // DB timeouts (statement-level, separate from our async tier timeouts)
    pub db_statement_timeout_ms: u64,
    pub db_idle_timeout_secs: u64,
    pub db_long_running_statement_timeout_ms: u64,
    // Processor pool
    pub processor_workers: usize,
    pub processor_batch_size: u32,
    pub processor_poll_interval_ms: u64,
    // Adaptive batch sizing
    pub processor_min_batch: u32,
    pub processor_max_batch: u32,
    pub processor_scaling_factor: f64,
    // Slow query logging
    pub slow_query_threshold_ms: u64,
    // Settlement batch limits
    pub settlement_max_batch_size: usize,
    pub settlement_min_tx_count: usize,
    // Partition configuration
    pub partition_lookahead_months: u32,
    pub partition_lookahead_min: u32,
    pub partition_lookahead_max: u32,
    // Per-class query timeout configuration
    pub per_class_timeout_config: PerClassTimeoutConfig,
    // Cross-chain bridge handoff configuration
    /// Whether settled swaps are handed off to the cross-chain bridge phase.
    /// When disabled, no handoff records are produced (local/no-op mode).
    pub bridge_enabled: bool,
    /// Base URL of the bridge adapter endpoint. Optional: when unset the
    /// no-op/local bridge implementation is used.
    pub bridge_endpoint_url: Option<String>,
    /// How long to wait for a bridge acknowledgement before marking a
    /// handoff as failed (and eligible for retry/replay), in seconds.
    pub bridge_ack_timeout_secs: u64,
    // Graceful shutdown configuration
    /// Total grace period allowed for in-flight swap work to drain before
    /// the process proceeds to flush the outbox and close the DB pool.
    pub shutdown_grace_period_secs: u64,
    /// Maximum time to wait for in-flight swap executions to finish during
    /// draining, in seconds. Must not exceed `shutdown_grace_period_secs`.
    pub shutdown_drain_timeout_secs: u64,
}

pub mod assets;
impl Config {
    pub async fn load() -> anyhow::Result<Self> {
        // Determine profile before loading env files
        let app_env = AppEnv::from_str(
            &std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string()),
        );

        // Load base .env then profile-specific .env.{profile}
        load_env_profile(&app_env);

        // Apply profile defaults for any unset vars
        apply_profile_defaults(&app_env);

        tracing::info!("Active environment profile: {}", app_env.as_str());

        let allowed_ips =
            parse_allowed_ips(&env::var("ALLOWED_IPS").unwrap_or_else(|_| "*".to_string()))?;

        let log_format =
            parse_log_format(&env::var("LOG_FORMAT").unwrap_or_else(|_| "text".to_string()))?;

        let use_vault = env::var("VAULT_ROLE_ID").is_ok() && env::var("VAULT_SECRET_ID").is_ok();

        let (database_url, anchor_webhook_secret) = if use_vault {
            let secrets = SecretsManager::new().await?;
            let db_password = secrets.get_db_password().await?;
            let anchor_secret = secrets.get_anchor_secret().await?;

            let db_template = env::var("DATABASE_URL_TEMPLATE").ok();
            let db_url = db_template
                .map(|template| template.replace("{password}", &db_password))
                .unwrap_or_else(|| env::var("DATABASE_URL").unwrap_or_default());

            (db_url, anchor_secret)
        } else {
            (
                env::var("DATABASE_URL")?,
                env::var("ANCHOR_WEBHOOK_SECRET")?,
            )
        };

        Ok(Config {
            app_env,
            server_port: env::var("SERVER_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()?,
            database_url,
            database_replica_url: env::var("DATABASE_REPLICA_URL").ok(),
            backup_database_url: env::var("BACKUP_DATABASE_URL").ok(),
            stellar_horizon_url: env::var("STELLAR_HORIZON_URL")?,
            anchor_webhook_secret,
            redis_url: env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://localhost:6379".to_string()),
            default_rate_limit: env::var("DEFAULT_RATE_LIMIT")
                .unwrap_or_else(|_| "100".to_string())
                .parse()?,
            whitelist_rate_limit: env::var("WHITELIST_RATE_LIMIT")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()?,
            whitelisted_ips: env::var("WHITELISTED_IPS").unwrap_or_default(),
            log_format,
            allowed_ips,
            // Default of 1 assumes a single trusted proxy/load balancer in front of
            // the app (the common case for this deployment shape). A deployment with
            // a different topology (e.g. no proxy, or a proxy chain) must set this
            // explicitly rather than relying on the default.
            trusted_proxy_depth: env::var("TRUSTED_PROXY_DEPTH")
                .unwrap_or_else(|_| "1".to_string())
                .parse()?,
            backup_dir: env::var("BACKUP_DIR").unwrap_or_else(|_| "./backups".to_string()),
            backup_encryption_key: env::var("BACKUP_ENCRYPTION_KEY").ok(),
            db_timeouts: DbTimeoutConfig {
                read_query_secs: env::var("DB_TIMEOUT_READ_SECS")
                    .unwrap_or_else(|_| "5".to_string())
                    .parse()
                    .unwrap_or(5),
                write_query_secs: env::var("DB_TIMEOUT_WRITE_SECS")
                    .unwrap_or_else(|_| "10".to_string())
                    .parse()
                    .unwrap_or(10),
                admin_query_secs: env::var("DB_TIMEOUT_ADMIN_SECS")
                    .unwrap_or_else(|_| "60".to_string())
                    .parse()
                    .unwrap_or(60),
            },
            otlp_endpoint: env::var("OTLP_ENDPOINT").ok(),
            cors_allowed_origins: env::var("CORS_ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            max_pending_queue: env::var("MAX_PENDING_QUEUE")
                .unwrap_or_else(|_| "10000".to_string())
                .parse()?,
            db_min_connections: env::var("DB_MIN_CONNECTIONS")
                .unwrap_or_else(|_| "5".to_string())
                .parse()?,
            db_max_connections: env::var("DB_MAX_CONNECTIONS")
                .unwrap_or_else(|_| "50".to_string())
                .parse()?,
            db_statement_timeout_ms: env::var("DB_STATEMENT_TIMEOUT_MS")
                .unwrap_or_else(|_| "30000".to_string())
                .parse()?,
            db_idle_timeout_secs: env::var("DB_IDLE_TIMEOUT_SECS")
                .unwrap_or_else(|_| "600".to_string())
                .parse()?,
            db_long_running_statement_timeout_ms: env::var("DB_LONG_RUNNING_STATEMENT_TIMEOUT_MS")
                .unwrap_or_else(|_| "300000".to_string())
                .parse()?,
            processor_workers: env::var("PROCESSOR_WORKERS")
                .unwrap_or_else(|_| "4".to_string())
                .parse()?,
            processor_batch_size: env::var("PROCESSOR_BATCH_SIZE")
                .unwrap_or_else(|_| "50".to_string())
                .parse()?,
            processor_poll_interval_ms: env::var("PROCESSOR_POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()?,
            processor_min_batch: env::var("PROCESSOR_MIN_BATCH")
                .unwrap_or_else(|_| "10".to_string())
                .parse()?,
            processor_max_batch: env::var("PROCESSOR_MAX_BATCH")
                .unwrap_or_else(|_| "500".to_string())
                .parse()?,
            processor_scaling_factor: env::var("PROCESSOR_SCALING_FACTOR")
                .unwrap_or_else(|_| "0.5".to_string())
                .parse()?,
            slow_query_threshold_ms: env::var("SLOW_QUERY_THRESHOLD_MS")
                .unwrap_or_else(|_| "500".to_string())
                .parse()?,
            settlement_max_batch_size: env::var("SETTLEMENT_MAX_BATCH_SIZE")
                .unwrap_or_else(|_| "10000".to_string())
                .parse()?,
            settlement_min_tx_count: env::var("SETTLEMENT_MIN_TX_COUNT")
                .unwrap_or_else(|_| "1".to_string())
                .parse()?,
            partition_lookahead_min: env::var("PARTITION_LOOKAHEAD_MIN")
                .unwrap_or_else(|_| "1".to_string())
                .parse()?,
            partition_lookahead_max: env::var("PARTITION_LOOKAHEAD_MAX")
                .unwrap_or_else(|_| "12".to_string())
                .parse()?,
            partition_lookahead_months: {
                let min: u32 = env::var("PARTITION_LOOKAHEAD_MIN")
                    .unwrap_or_else(|_| "1".to_string())
                    .parse()
                    .unwrap_or(1);
                let max: u32 = env::var("PARTITION_LOOKAHEAD_MAX")
                    .unwrap_or_else(|_| "12".to_string())
                    .parse()
                    .unwrap_or(12);
                let lookahead: u32 = env::var("PARTITION_LOOKAHEAD_MONTHS")
                    .unwrap_or_else(|_| "3".to_string())
                    .parse()?;

                if lookahead < min {
                    anyhow::bail!("PARTITION_LOOKAHEAD_MONTHS ({}) cannot be less than PARTITION_LOOKAHEAD_MIN ({})", lookahead, min);
                }
                if lookahead > max {
                    anyhow::bail!("PARTITION_LOOKAHEAD_MONTHS ({}) cannot be greater than PARTITION_LOOKAHEAD_MAX ({})", lookahead, max);
                }
                lookahead
            },
            per_class_timeout_config: PerClassTimeoutConfig {
                interactive_timeout_ms: env::var("DB_TIMEOUT_INTERACTIVE_MS")
                    .unwrap_or_else(|_| "5000".to_string())
                    .parse()?,
                admin_report_timeout_ms: env::var("DB_TIMEOUT_ADMIN_REPORT_MS")
                    .unwrap_or_else(|_| "30000".to_string())
                    .parse()?,
                background_job_timeout_ms: env::var("DB_TIMEOUT_BACKGROUND_JOB_MS")
                    .unwrap_or_else(|_| "60000".to_string())
                    .parse()?,
            },
            bridge_enabled: env::var("BRIDGE_ENABLED")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),
            bridge_endpoint_url: env::var("BRIDGE_ENDPOINT_URL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            bridge_ack_timeout_secs: env::var("BRIDGE_ACK_TIMEOUT_SECS")
                .unwrap_or_else(|_| "300".to_string())
                .parse()?,
            shutdown_grace_period_secs: env::var("SHUTDOWN_GRACE_PERIOD_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()?,
            shutdown_drain_timeout_secs: {
                let grace: u64 = env::var("SHUTDOWN_GRACE_PERIOD_SECS")
                    .unwrap_or_else(|_| "30".to_string())
                    .parse()
                    .unwrap_or(30);
                let drain: u64 = env::var("SHUTDOWN_DRAIN_TIMEOUT_SECS")
                    .unwrap_or_else(|_| "20".to_string())
                    .parse()?;
                if drain > grace {
                    anyhow::bail!(
                        "SHUTDOWN_DRAIN_TIMEOUT_SECS ({}) cannot be greater than SHUTDOWN_GRACE_PERIOD_SECS ({})",
                        drain,
                        grace
                    );
                }
                drain
            },
        })
    }
}

fn parse_allowed_ips(raw: &str) -> anyhow::Result<AllowedIps> {
    let value = raw.trim();
    if value == "*" {
        return Ok(AllowedIps::Any);
    }

    let cidrs = value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::parse::<IpNet>)
        .collect::<Result<Vec<_>, _>>()?;

    if cidrs.is_empty() {
        anyhow::bail!("ALLOWED_IPS must be '*' or a comma-separated list of CIDRs");
    }

    Ok(AllowedIps::Cidrs(cidrs))
}

fn parse_log_format(raw: &str) -> anyhow::Result<LogFormat> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => anyhow::bail!("LOG_FORMAT must be 'text' or 'json'"),
    }
}

/// Settings for the swap quoting engine (`src/swap/quote.rs`).
///
/// Kept separate from [`Config`] so the quoting layer can be built and tested
/// without the full application configuration.
#[derive(Debug, Clone)]
pub struct SwapQuoteConfig {
    /// Platform fee in basis points, charged on the output after source spread.
    /// `SWAP_FEE_BPS`, default `0`, must be within `0..=10000`.
    pub fee_bps: u32,
    /// How long an issued quote stays executable. `SWAP_QUOTE_TTL_SECONDS`,
    /// default `30`, must be greater than zero.
    pub quote_ttl_seconds: u64,
    /// Per-source timeout applied to each price fan-out call.
    /// `SWAP_SOURCE_TIMEOUT_MS`, default `2000`.
    pub source_timeout_ms: u64,
    /// Smallest accepted order, in base-asset units. `SWAP_MIN_ORDER_SIZE`, default `1`.
    pub min_order_size: sqlx::types::BigDecimal,
    /// Largest accepted order, in base-asset units. `SWAP_MAX_ORDER_SIZE`, default `1000000`.
    pub max_order_size: sqlx::types::BigDecimal,
    /// Maximum number of ranked candidates persisted per quote.
    /// `SWAP_MAX_QUOTE_CANDIDATES`, default `10`, must be at least one.
    pub max_candidates: usize,
}

impl SwapQuoteConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let config = Self {
            fee_bps: env::var("SWAP_FEE_BPS")
                .unwrap_or_else(|_| "0".to_string())
                .parse()?,
            quote_ttl_seconds: env::var("SWAP_QUOTE_TTL_SECONDS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()?,
            source_timeout_ms: env::var("SWAP_SOURCE_TIMEOUT_MS")
                .unwrap_or_else(|_| "2000".to_string())
                .parse()?,
            min_order_size: env::var("SWAP_MIN_ORDER_SIZE")
                .unwrap_or_else(|_| "1".to_string())
                .parse()?,
            max_order_size: env::var("SWAP_MAX_ORDER_SIZE")
                .unwrap_or_else(|_| "1000000".to_string())
                .parse()?,
            max_candidates: env::var("SWAP_MAX_QUOTE_CANDIDATES")
                .unwrap_or_else(|_| "10".to_string())
                .parse()?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.fee_bps > 10_000 {
            anyhow::bail!("SWAP_FEE_BPS must be within 0..=10000");
        }
        if self.quote_ttl_seconds == 0 {
            anyhow::bail!("SWAP_QUOTE_TTL_SECONDS must be greater than zero");
        }
        if self.max_candidates == 0 {
            anyhow::bail!("SWAP_MAX_QUOTE_CANDIDATES must be at least one");
        }
        let zero = sqlx::types::BigDecimal::from(0);
        if self.min_order_size <= zero || self.min_order_size > self.max_order_size {
            anyhow::bail!(
                "SWAP_MIN_ORDER_SIZE must be positive and not exceed SWAP_MAX_ORDER_SIZE"
            );
        }
        Ok(())
    }
}

/// Pre-trade risk limits for swap orders, evaluated by
/// [`crate::swap::risk::RiskPolicy`].
///
/// Loaded once via [`SwapRiskConfig::from_env`]. Every limit except the
/// kill switch and the allow/deny lists is required, and any value that is
/// missing or unparseable is a startup error (fail closed).
#[derive(Debug, Clone, PartialEq)]
pub struct SwapRiskConfig {
    /// `SWAP_KILL_SWITCH`: when `true`, every swap order is rejected. Default `false`.
    pub kill_switch: bool,
    /// `SWAP_ALLOWED_PAIRS` (e.g. `USDC>XLM,XLM>USDC`): the only permitted
    /// source/target asset pairs. Required and non-empty.
    pub allowed_pairs: Vec<(String, String)>,
    /// `SWAP_MAX_ORDER_AMOUNT`: maximum amount of a single order, in source-asset units.
    pub max_order_amount: BigDecimal,
    /// `SWAP_MAX_ACCOUNT_VOLUME`: maximum rolling volume per account over the window.
    pub max_account_volume: BigDecimal,
    /// `SWAP_MAX_TENANT_VOLUME`: maximum rolling volume per tenant over the window.
    pub max_tenant_volume: BigDecimal,
    /// `SWAP_ROLLING_WINDOW_SECS`: length of the rolling volume window, in seconds.
    pub rolling_window_secs: i64,
    /// `SWAP_ACCOUNT_ALLOWLIST`: optional. When non-empty, only listed accounts may swap.
    pub account_allowlist: Vec<String>,
    /// `SWAP_ACCOUNT_DENYLIST`: optional. Listed accounts are always rejected, even if allowlisted.
    pub account_denylist: Vec<String>,
}

impl SwapRiskConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let kill_switch = match env::var("SWAP_KILL_SWITCH") {
            Ok(v) => parse_bool("SWAP_KILL_SWITCH", &v)?,
            Err(_) => false,
        };

        let allowed_pairs = parse_pairs(&required_env("SWAP_ALLOWED_PAIRS")?)?;
        let max_order_amount = parse_positive_decimal(
            "SWAP_MAX_ORDER_AMOUNT",
            &required_env("SWAP_MAX_ORDER_AMOUNT")?,
        )?;
        let max_account_volume = parse_positive_decimal(
            "SWAP_MAX_ACCOUNT_VOLUME",
            &required_env("SWAP_MAX_ACCOUNT_VOLUME")?,
        )?;
        let max_tenant_volume = parse_positive_decimal(
            "SWAP_MAX_TENANT_VOLUME",
            &required_env("SWAP_MAX_TENANT_VOLUME")?,
        )?;

        let rolling_window_secs: i64 = required_env("SWAP_ROLLING_WINDOW_SECS")?
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("SWAP_ROLLING_WINDOW_SECS must be an integer: {e}"))?;
        if rolling_window_secs <= 0 {
            anyhow::bail!("SWAP_ROLLING_WINDOW_SECS must be greater than 0");
        }

        Ok(Self {
            kill_switch,
            allowed_pairs,
            max_order_amount,
            max_account_volume,
            max_tenant_volume,
            rolling_window_secs,
            account_allowlist: parse_list(&env::var("SWAP_ACCOUNT_ALLOWLIST").unwrap_or_default()),
            account_denylist: parse_list(&env::var("SWAP_ACCOUNT_DENYLIST").unwrap_or_default()),
        })
    }
}

fn required_env(key: &str) -> anyhow::Result<String> {
    env::var(key).map_err(|_| anyhow::anyhow!("{key} is required"))
}

fn parse_bool(key: &str, raw: &str) -> anyhow::Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("{key} must be 'true' or 'false'"),
    }
}

fn parse_positive_decimal(key: &str, raw: &str) -> anyhow::Result<BigDecimal> {
    let value = BigDecimal::from_str(raw.trim())
        .map_err(|e| anyhow::anyhow!("{key} must be a decimal number: {e}"))?;
    if value <= BigDecimal::from(0) {
        anyhow::bail!("{key} must be greater than 0");
    }
    Ok(value)
}

fn parse_pairs(raw: &str) -> anyhow::Result<Vec<(String, String)>> {
    let pairs = raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| match entry.split_once('>') {
            Some((source, target)) if !source.trim().is_empty() && !target.trim().is_empty() => {
                Ok((source.trim().to_string(), target.trim().to_string()))
            }
            _ => anyhow::bail!("SWAP_ALLOWED_PAIRS entries must be 'SOURCE>TARGET', got '{entry}'"),
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    if pairs.is_empty() {
        anyhow::bail!("SWAP_ALLOWED_PAIRS must list at least one pair");
    }
    Ok(pairs)
}

fn parse_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(String::from)
        .collect()
}
