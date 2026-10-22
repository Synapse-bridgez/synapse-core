use crate::secrets::SecretsManager;
use anyhow::Result;
use dotenvy::dotenv;
use ipnet::IpNet;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

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
        unsafe { env::set_var(key, value) };
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

/// Authoritative clock source for time comparisons (calendar windows, quote
/// expiry, settlement scheduling). Production uses `SystemClock`; tests inject
/// a deterministic clock so time-dependent behavior is reproducible.
pub trait Clock: Send + Sync {
    /// Current time as Unix epoch seconds.
    fn now_unix_secs(&self) -> i64;
}

/// Wall-clock implementation backed by `SystemTime`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// A clock pinned to a fixed instant, for deterministic tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    pub unix_secs: i64,
}

impl Clock for FixedClock {
    fn now_unix_secs(&self) -> i64 {
        self.unix_secs
    }
}

/// Dynamic Stellar inclusion-fee strategy configuration.
///
/// All fee amounts are expressed in stroops (1 XLM = 10_000_000 stroops) and
/// carried as `i64` end-to-end to match the settlement submission path.
#[derive(Debug, Clone)]
pub struct FeeStrategyConfig {
    /// How often Horizon fee statistics are polled, in milliseconds.
    pub poll_interval_ms: u64,
    /// Percentile of recent inclusion fees to target (0-100).
    pub percentile: u8,
    /// Lower bound on the recommended inclusion fee, in stroops.
    pub min_fee_stroops: i64,
    /// Upper bound on the recommended inclusion fee, in stroops.
    pub max_fee_stroops: i64,
    /// Per-tenant cap on total fee spend per settlement, in stroops.
    pub per_tenant_cap_stroops: i64,
    /// Global cap on total fee spend per settlement, in stroops.
    pub global_cap_stroops: i64,
    /// How long a submitted transaction may remain un-included before a bump
    /// is attempted, in seconds.
    pub stalled_timeout_secs: u64,
    /// Maximum number of fee-bump attempts per settlement transaction.
    pub max_bump_attempts: u32,
}

impl Default for FeeStrategyConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 5_000,
            percentile: 90,
            min_fee_stroops: 100,
            max_fee_stroops: 100_000,
            per_tenant_cap_stroops: 1_000_000,
            global_cap_stroops: 10_000_000,
            stalled_timeout_secs: 30,
            max_bump_attempts: 3,
        }
    }
}

impl FeeStrategyConfig {
    /// Validate bounds and caps, returning an actionable error on misconfiguration.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.poll_interval_ms == 0 {
            anyhow::bail!("FEE_POLL_INTERVAL_MS must be greater than 0");
        }
        if self.percentile > 100 {
            anyhow::bail!("FEE_PERCENTILE must be between 0 and 100");
        }
        if self.min_fee_stroops <= 0 {
            anyhow::bail!("FEE_MIN_STROOPS must be greater than 0");
        }
        if self.max_fee_stroops < self.min_fee_stroops {
            anyhow::bail!(
                "FEE_MAX_STROOPS ({}) cannot be less than FEE_MIN_STROOPS ({})",
                self.max_fee_stroops,
                self.min_fee_stroops
            );
        }
        if self.per_tenant_cap_stroops <= 0 {
            anyhow::bail!("FEE_PER_TENANT_CAP_STROOPS must be greater than 0");
        }
        if self.global_cap_stroops <= 0 {
            anyhow::bail!("FEE_GLOBAL_CAP_STROOPS must be greater than 0");
        }
        if self.per_tenant_cap_stroops > self.global_cap_stroops {
            anyhow::bail!(
                "FEE_PER_TENANT_CAP_STROOPS ({}) cannot exceed FEE_GLOBAL_CAP_STROOPS ({})",
                self.per_tenant_cap_stroops,
                self.global_cap_stroops
            );
        }
        if self.stalled_timeout_secs == 0 {
            anyhow::bail!("FEE_STALLED_TIMEOUT_SECS must be greater than 0");
        }
        if self.max_bump_attempts == 0 {
            anyhow::bail!("FEE_MAX_BUMP_ATTEMPTS must be greater than 0");
        }
        Ok(())
    }
}

/// Weekly trading-hours window for a single day, expressed as minutes since
/// midnight in the calendar's IANA timezone. `start < end`; `None` means closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TradingWindow {
    pub start_minute: u16,
    pub end_minute: u16,
}

impl TradingWindow {
    /// Parse a `HH:MM-HH:MM` window string.
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let (start, end) = raw
            .split_once('-')
            .ok_or_else(|| anyhow::anyhow!("trading window '{}' must be HH:MM-HH:MM", raw))?;
        let start_minute = parse_hhmm(start)?;
        let end_minute = parse_hhmm(end)?;
        if start_minute >= end_minute {
            anyhow::bail!("trading window '{}' start must be before end", raw);
        }
        Ok(Self {
            start_minute,
            end_minute,
        })
    }
}

fn parse_hhmm(raw: &str) -> anyhow::Result<u16> {
    let (h, m) = raw
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("time '{}' must be HH:MM", raw))?;
    let hour: u16 = h
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid hour in '{}'", raw))?;
    let minute: u16 = m
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid minute in '{}'", raw))?;
    if hour > 23 || minute > 59 {
        anyhow::bail!("time '{}' is out of range", raw);
    }
    Ok(hour * 60 + minute)
}

/// A single venue/corridor trading calendar.
#[derive(Debug, Clone)]
pub struct CalendarDefinition {
    /// Venue or corridor identifier this calendar applies to.
    pub id: String,
    /// IANA timezone name (e.g. `America/New_York`).
    pub timezone: String,
    /// Weekly hours indexed by ISO weekday (0 = Monday .. 6 = Sunday).
    pub weekly_hours: [Option<TradingWindow>; 7],
    /// Holiday exceptions as `YYYY-MM-DD` dates on which trading is closed.
    pub holidays: Vec<String>,
    /// Monotonic version identifier for this calendar definition.
    pub version: u64,
}

impl CalendarDefinition {
    /// Validate timezone presence, holiday date format, and window bounds.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.id.trim().is_empty() {
            anyhow::bail!("calendar id must not be empty");
        }
        if self.timezone.trim().is_empty() {
            anyhow::bail!("calendar '{}' must specify an IANA timezone", self.id);
        }
        for holiday in &self.holidays {
            validate_iso_date(holiday)
                .map_err(|e| anyhow::anyhow!("calendar '{}' holiday: {}", self.id, e))?;
        }
        Ok(())
    }
}

fn validate_iso_date(raw: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = raw.trim().split('-').collect();
    if parts.len() != 3 {
        anyhow::bail!("date '{}' must be YYYY-MM-DD", raw);
    }
    let year: i32 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid year in '{}'", raw))?;
    let month: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid month in '{}'", raw))?;
    let day: u32 = parts[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid day in '{}'", raw))?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1970 {
        anyhow::bail!("date '{}' is out of range", raw);
    }
    Ok(())
}

/// Immutable, versioned collection of trading calendars plus the authoritative
/// clock used for all time comparisons.
#[derive(Debug, Clone)]
pub struct CalendarConfig {
    /// Calendars keyed by venue/corridor id.
    pub calendars: HashMap<String, CalendarDefinition>,
    /// Global version identifier for the loaded calendar set.
    pub version: u64,
    /// Authoritative clock source for time comparisons.
    pub clock: Arc<dyn Clock>,
}

impl CalendarConfig {
    /// Build a calendar config from definitions, validating each and assigning
    /// the given version. Uses the system clock by default.
    pub fn new(
        calendars: Vec<CalendarDefinition>,
        version: u64,
    ) -> anyhow::Result<Self> {
        let mut map = HashMap::new();
        for cal in calendars {
            cal.validate()?;
            if map.insert(cal.id.clone(), cal).is_some() {
                anyhow::bail!("duplicate calendar id");
            }
        }
        Ok(Self {
            calendars: map,
            version,
            clock: Arc::new(SystemClock),
        })
    }

    /// Replace the authoritative clock (used by tests for determinism).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Current time from the authoritative clock, in Unix epoch seconds.
    pub fn now_unix_secs(&self) -> i64 {
        self.clock.now_unix_secs()
    }

    /// Look up a calendar by venue/corridor id.
    pub fn get(&self, id: &str) -> Option<&CalendarDefinition> {
        self.calendars.get(id)
    }
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            calendars: HashMap::new(),
            version: 0,
            clock: Arc::new(SystemClock),
        }
    }
}

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
    // Dynamic Stellar inclusion-fee strategy
    pub fee_strategy: FeeStrategyConfig,
    // Trading calendars, market hours, and settlement windows
    pub calendars: CalendarConfig,
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
            fee_strategy: load_fee_strategy()?,
            calendars: load_calendars()?,
        })
    }
}

/// Load the dynamic fee strategy configuration from the environment, applying
/// defaults for unset values and validating bounds/caps.
fn load_fee_strategy() -> anyhow::Result<FeeStrategyConfig> {
    let defaults = FeeStrategyConfig::default();
    let config = FeeStrategyConfig {
        poll_interval_ms: env::var("FEE_POLL_INTERVAL_MS")
            .unwrap_or_else(|_| defaults.poll_interval_ms.to_string())
            .parse()?,
        percentile: env::var("FEE_PERCENTILE")
            .unwrap_or_else(|_| defaults.percentile.to_string())
            .parse()?,
        min_fee_stroops: env::var("FEE_MIN_STROOPS")
            .unwrap_or_else(|_| defaults.min_fee_stroops.to_string())
            .parse()?,
        max_fee_stroops: env::var("FEE_MAX_STROOPS")
            .unwrap_or_else(|_| defaults.max_fee_stroops.to_string())
            .parse()?,
        per_tenant_cap_stroops: env::var("FEE_PER_TENANT_CAP_STROOPS")
            .unwrap_or_else(|_| defaults.per_tenant_cap_stroops.to_string())
            .parse()?,
        global_cap_stroops: env::var("FEE_GLOBAL_CAP_STROOPS")
            .unwrap_or_else(|_| defaults.global_cap_stroops.to_string())
            .parse()?,
        stalled_timeout_secs: env::var("FEE_STALLED_TIMEOUT_SECS")
            .unwrap_or_else(|_| defaults.stalled_timeout_secs.to_string())
            .parse()?,
        max_bump_attempts: env::var("FEE_MAX_BUMP_ATTEMPTS")
            .unwrap_or_else(|_| defaults.max_bump_attempts.to_string())
            .parse()?,
    };
    config.validate()?;
    Ok(config)
}

/// Load trading calendars from the environment.
///
/// `CALENDARS` is a comma-separated list of calendar ids. For each id `<ID>`,
/// the following variables are read:
/// - `CALENDAR_<ID>_TZ` — IANA timezone (required)
/// - `CALENDAR_<ID>_VERSION` — version identifier (default: 1)
/// - `CALENDAR_<ID>_HOURS` — comma-separated `DAY=HH:MM-HH:MM` entries where
///   `DAY` is `mon`..`sun`
/// - `CALENDAR_<ID>_HOLIDAYS` — comma-separated `YYYY-MM-DD` dates
fn load_calendars() -> anyhow::Result<CalendarConfig> {
    let raw = env::var("CALENDARS").unwrap_or_default();
    let ids: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut definitions = Vec::with_capacity(ids.len());
    for id in ids {
        let key = id.to_ascii_uppercase().replace(['-', '.'], "_");
        let timezone = env::var(format!("CALENDAR_{}_TZ", key))
            .map_err(|_| anyhow::anyhow!("calendar '{}' is missing CALENDAR_{}_TZ", id, key))?;
        let version: u64 = env::var(format!("CALENDAR_{}_VERSION", key))
            .unwrap_or_else(|_| "1".to_string())
            .parse()?;

        let mut weekly_hours: [Option<TradingWindow>; 7] = [None; 7];
        if let Ok(hours) = env::var(format!("CALENDAR_{}_HOURS", key)) {
            for entry in hours.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let (day, window) = entry.split_once('=').ok_or_else(|| {
                    anyhow::anyhow!("calendar '{}' hours entry '{}' must be DAY=HH:MM-HH:MM", id, entry)
                })?;
                let idx = weekday_index(day).ok_or_else(|| {
                    anyhow::anyhow!("calendar '{}' has unknown day '{}'", id, day)
                })?;
                weekly_hours[idx] = Some(TradingWindow::parse(window)?);
            }
        }

        let holidays = env::var(format!("CALENDAR_{}_HOLIDAYS", key))
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        definitions.push(CalendarDefinition {
            id: id.to_string(),
            timezone,
            weekly_hours,
            holidays,
            version,
        });
    }

    let version: u64 = env::var("CALENDARS_VERSION")
        .unwrap_or_else(|_| "1".to_string())
        .parse()?;

    CalendarConfig::new(definitions, version)
}

fn weekday_index(day: &str) -> Option<usize> {
    match day.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Some(0),
        "tue" | "tuesday" => Some(1),
        "wed" | "wednesday" => Some(2),
        "thu" | "thursday" => Some(3),
        "fri" | "friday" => Some(4),
        "sat" | "saturday" => Some(5),
        "sun" | "sunday" => Some(6),
        _ => None,
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
