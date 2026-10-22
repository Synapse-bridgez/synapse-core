use crate::secrets::SecretsManager;
use crate::swap::fees::{FeeConfig, FeeRule};
use crate::swap::circuit_breaker::BreakerConfig;
use anyhow::Result;
use dotenvy::dotenv;
use ipnet::IpNet;
use sqlx::types::BigDecimal;
use std::env;
use std::str::FromStr;
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

/// Market-data ingestion settings for swap quoting (see `swap::market_data`).
///
/// Freshness: a cached price is usable for quoting only while its source
/// timestamp is at most `max_age_secs` old. `cache_ttl_secs` bounds how long a
/// fresh cache entry lives; a last-known copy is kept beyond it for stale fallback.
#[derive(Debug, Clone)]
pub struct MarketDataConfig {
    /// Base URL of the HTTP price source (`PRICE_SOURCE_URL`). Unset disables HTTP ingestion.
    pub source_url: Option<String>,
    /// Per-request timeout in milliseconds (`PRICE_SOURCE_TIMEOUT_MS`). Default: 2000
    pub source_timeout_ms: u64,
    /// Retries after the first failed attempt (`PRICE_SOURCE_MAX_RETRIES`). Default: 2, max 10
    pub source_max_retries: u32,
    /// Base backoff between retries, doubled per attempt (`PRICE_SOURCE_RETRY_BACKOFF_MS`). Default: 200
    pub source_retry_backoff_ms: u64,
    /// Interval between scheduled ingestion runs (`PRICE_INGESTION_INTERVAL_SECS`). Default: 10
    pub ingestion_interval_secs: u64,
    /// TTL of the fresh snapshot cache entry (`PRICE_CACHE_TTL_SECS`). Default: 30
    pub cache_ttl_secs: u64,
    /// Maximum snapshot age accepted for quoting (`PRICE_MAX_AGE_SECS`). Default: 15
    pub max_age_secs: u64,
}

impl MarketDataConfig {
    fn from_env() -> anyhow::Result<Self> {
        let config = Self {
            source_url: env::var("PRICE_SOURCE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            source_timeout_ms: env::var("PRICE_SOURCE_TIMEOUT_MS")
                .unwrap_or_else(|_| "2000".to_string())
                .parse()?,
            source_max_retries: env::var("PRICE_SOURCE_MAX_RETRIES")
                .unwrap_or_else(|_| "2".to_string())
                .parse()?,
            source_retry_backoff_ms: env::var("PRICE_SOURCE_RETRY_BACKOFF_MS")
                .unwrap_or_else(|_| "200".to_string())
                .parse()?,
            ingestion_interval_secs: env::var("PRICE_INGESTION_INTERVAL_SECS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()?,
            cache_ttl_secs: env::var("PRICE_CACHE_TTL_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()?,
            max_age_secs: env::var("PRICE_MAX_AGE_SECS")
                .unwrap_or_else(|_| "15".to_string())
                .parse()?,
        };

        if let Some(url) = &config.source_url {
            url::Url::parse(url)?;
        }
        anyhow::ensure!(config.source_timeout_ms > 0, "PRICE_SOURCE_TIMEOUT_MS must be > 0");
        anyhow::ensure!(
            config.source_max_retries <= 10,
            "PRICE_SOURCE_MAX_RETRIES must be <= 10"
        );
        anyhow::ensure!(
            config.ingestion_interval_secs > 0,
            "PRICE_INGESTION_INTERVAL_SECS must be > 0"
        );
        anyhow::ensure!(config.cache_ttl_secs > 0, "PRICE_CACHE_TTL_SECS must be > 0");
        anyhow::ensure!(config.max_age_secs > 0, "PRICE_MAX_AGE_SECS must be > 0");

        Ok(config)
    }
}

/// Phase 2 swap engine settings. All variables are optional; the engine is
/// disabled by default and nothing in `src/swap` is wired while it is off.
#[derive(Debug, Clone)]
pub struct SwapConfig {
    /// Master switch for the swap engine (`SWAP_ENGINE_ENABLED`). Default: false
    pub engine_enabled: bool,
    /// Largest order accepted, in source-asset units (`SWAP_MAX_ORDER_AMOUNT`). Default: 10000
    pub max_order_amount: BigDecimal,
    /// Engine fee in basis points, 1 bp = 0.01% (`SWAP_FEE_BPS`). Default: 0
    pub fee_bps: u32,
    /// Lifetime of an issued quote in seconds (`SWAP_QUOTE_TTL_SECONDS`). Default: 30
    pub quote_ttl_seconds: u64,
}

impl Default for SwapConfig {
    fn default() -> Self {
        Self {
            engine_enabled: false,
            max_order_amount: BigDecimal::from(10_000),
            fee_bps: 0,
            quote_ttl_seconds: 30,
        }
    }
}

impl SwapConfig {
    /// Load swap settings from the environment, falling back to
    /// [`SwapConfig::default`] for any variable that is unset. Invalid values
    /// fail startup and name the offending variable.
    pub fn from_env() -> anyhow::Result<Self> {
        let defaults = Self::default();
        let config = Self {
            engine_enabled: parse_bool_env("SWAP_ENGINE_ENABLED", defaults.engine_enabled)?,
            max_order_amount: parse_env_or("SWAP_MAX_ORDER_AMOUNT", defaults.max_order_amount)?,
            fee_bps: parse_env_or("SWAP_FEE_BPS", defaults.fee_bps)?,
            quote_ttl_seconds: parse_env_or("SWAP_QUOTE_TTL_SECONDS", defaults.quote_ttl_seconds)?,
        };

        if config.max_order_amount <= BigDecimal::from(0) {
            anyhow::bail!("SWAP_MAX_ORDER_AMOUNT must be greater than 0");
        }
        if config.fee_bps > 10_000 {
            anyhow::bail!("SWAP_FEE_BPS must be between 0 and 10000 (100%)");
        }
        if config.quote_ttl_seconds == 0 {
            anyhow::bail!("SWAP_QUOTE_TTL_SECONDS must be greater than 0");
        }

        Ok(config)
    }
}

        Ok(config)
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
    // Stuck-transaction watchdog (src/swap/recovery.rs)
    pub stuck_processing_threshold_secs: u64,
    pub watchdog_interval_secs: u64,
    pub dlq_depth_alert_threshold: i64,
    pub dlq_age_alert_secs: u64,
    // Months of swap order/quote/fill/event partitions to retain before detaching
    pub swap_retention_months: i32,
    // Per-class query timeout configuration
    pub per_class_timeout_config: PerClassTimeoutConfig,
    /// How often the asset registry reloads from Postgres, in seconds.
    /// Failed reloads keep the last known good registry.
    pub asset_registry_refresh_secs: u64,
    /// Global swap order intake switch (`SWAP_ACCEPTING_ORDERS`). When false,
    /// new swap orders are rejected; in-flight orders are unaffected.
    pub swap_accepting_orders: bool,
    /// Per-tenant and per-provider swap circuit breaker thresholds.
    pub swap_breaker: BreakerConfig,
    // Swap engine rollout gate (process-wide kill switch + tenant allowlist)
    pub swap_engine_enabled: bool,
    pub swap_engine_tenant_allowlist: Vec<String>,
    // Swap fee defaults (per-tenant/pair overrides are layered on at runtime)
    pub swap_fees: FeeConfig,
    // Swap quote cache
    /// TTL for cached swap quotes, in seconds. Default: 10
    pub quote_cache_ttl_secs: u64,
    /// Max age of the price snapshot a cached quote was derived from, in seconds. Default: 5
    pub quote_cache_max_staleness_secs: u64,
    // Phase 2 swap engine (disabled by default)
    pub swap: SwapConfig,
    // Market data ingestion for swap quoting
    pub market_data: MarketDataConfig,
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
            swap_retention_months: env::var("SWAP_RETENTION_MONTHS")
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
            stuck_processing_threshold_secs: env::var("STUCK_PROCESSING_THRESHOLD_SECS")
                .unwrap_or_else(|_| "900".to_string())
                .parse()?,
            watchdog_interval_secs: env::var("WATCHDOG_INTERVAL_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()?,
            dlq_depth_alert_threshold: env::var("DLQ_DEPTH_ALERT_THRESHOLD")
                .unwrap_or_else(|_| "100".to_string())
                .parse()?,
            dlq_age_alert_secs: env::var("DLQ_AGE_ALERT_SECS")
                .unwrap_or_else(|_| "3600".to_string())
                .parse()?,
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
            market_data: MarketDataConfig::from_env()?,
            swap: SwapConfig::from_env()?,
        })
    }

    /// Env-level gate for new swap hand-offs. `false` when the process-wide kill
    /// switch (`SWAP_ENGINE_ENABLED`) is off, or when the tenant is not listed in
    /// `SWAP_ENGINE_TENANT_ALLOWLIST` (`*` admits every tenant). Callers must
    /// also check the per-tenant `swap_engine` feature flag. Only gates *new*
    /// hand-offs; orders already routed are not affected.
    pub fn swap_engine_allows_tenant(&self, tenant_id: &str) -> bool {
        self.swap_engine_enabled
            && self
                .swap_engine_tenant_allowlist
                .iter()
                .any(|entry| entry == "*" || entry == tenant_id)
    }
}

fn parse_bool(raw: &str, key: &str) -> anyhow::Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(true),
        "false" | "0" | "" => Ok(false),
        _ => anyhow::bail!("{key} must be 'true' or 'false'"),
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

/// Parse an optional env var, returning `default` when it is unset. A bad value
/// is reported with the variable name so startup failures are easy to trace.
fn parse_env_or<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(raw) => raw
            .trim()
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("{key} has invalid value '{raw}': {e}")),
        Err(_) => Ok(default),
    }
}

/// Parse an optional boolean env var. Only `true`/`false` and `1`/`0`
/// (case-insensitive) are accepted; anything else is rejected at startup
/// rather than guessed at.
fn parse_bool_env(key: &str, default: bool) -> anyhow::Result<bool> {
    match env::var(key) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => anyhow::bail!("{key} must be 'true', 'false', '1' or '0' (got '{raw}')"),
        },
        Err(_) => Ok(default),
    }
}

fn parse_log_format(raw: &str) -> anyhow::Result<LogFormat> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "text" => Ok(LogFormat::Text),
        "json" => Ok(LogFormat::Json),
        _ => anyhow::bail!("LOG_FORMAT must be 'text' or 'json'"),
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
