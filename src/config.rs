use crate::secrets::SecretsManager;
use anyhow::Result;
use dotenvy::dotenv;
use ipnet::IpNet;
use std::env;

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
            // LP adapter onboarding: certification not required in development
            set_default("LP_REQUIRE_CERTIFICATION", "false");
        }
        AppEnv::Staging => {
            set_default("LOG_FORMAT", "json");
            set_default("RUST_LOG", "info");
            set_default("DEFAULT_RATE_LIMIT", "500");
            set_default("WHITELIST_RATE_LIMIT", "5000");
            set_default("DB_TIMEOUT_READ_SECS", "10");
            set_default("DB_TIMEOUT_WRITE_SECS", "20");
            set_default("DB_STATEMENT_TIMEOUT_MS", "30000");
            // Staging mirrors production: certification required
            set_default("LP_REQUIRE_CERTIFICATION", "true");
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
            // Production: certification is mandatory before an adapter serves traffic
            set_default("LP_REQUIRE_CERTIFICATION", "true");
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

/// Where LP adapter credentials are sourced from.
///
/// Credentials are always resolved through [`SecretsManager`] (Vault) or the
/// process environment; they are never written to disk, logged, or embedded in
/// the manifest. The variant only selects *which* backend is consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LpCredentialSource {
    /// Read credentials from process environment variables.
    Environment,
    /// Read credentials from the secret store (Vault) via [`SecretsManager`].
    SecretStore,
}

impl LpCredentialSource {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "secret_store" | "secret-store" | "vault" => LpCredentialSource::SecretStore,
            _ => LpCredentialSource::Environment,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LpCredentialSource::Environment => "environment",
            LpCredentialSource::SecretStore => "secret_store",
        }
    }
}

/// Configuration for LP adapter onboarding, credentialing, and certification.
///
/// Note: this struct intentionally holds *no* secret material. Credentials are
/// fetched on demand through [`SecretsManager`] so that rotation does not
/// require a process restart and secrets never appear in `Debug` output.
#[derive(Debug, Clone)]
pub struct LpAdapterConfig {
    /// Directory containing adapter registration manifests (`*.toml`/`*.json`).
    pub manifest_dir: String,
    /// Where adapter credentials are loaded from.
    pub credential_source: LpCredentialSource,
    /// How often (in seconds) credentials are re-read to pick up rotations.
    /// Rotation is applied without a restart; `0` disables periodic refresh.
    pub credential_rotation_interval_secs: u64,
    /// Grace period (in seconds) during which the previous credential remains
    /// valid after a rotation, to avoid in-flight request failures.
    pub credential_rotation_grace_secs: u64,
    /// When `true`, an adapter must have a passing certification report before
    /// it is enabled outside test configurations.
    pub require_certification: bool,
    /// Directory where machine-readable certification reports are written.
    pub certification_report_dir: String,
}

impl Default for LpAdapterConfig {
    fn default() -> Self {
        Self {
            manifest_dir: "./config/lp-adapters".to_string(),
            credential_source: LpCredentialSource::Environment,
            credential_rotation_interval_secs: 300,
            credential_rotation_grace_secs: 60,
            require_certification: true,
            certification_report_dir: "./certification-reports".to_string(),
        }
    }
}

/// Configuration for Stellar settlement transaction submission.
///
/// Holds channel-account pooling and retry limits. Signed transaction
/// envelopes are never stored here (and must never be logged).
#[derive(Debug, Clone)]
pub struct StellarSettlementConfig {
    /// Source account used for settlement transactions when no channel account
    /// is available.
    pub source_account: Option<String>,
    /// Channel accounts available for pooling. Empty disables pooling.
    pub channel_accounts: Vec<String>,
    /// Maximum number of concurrent in-flight submissions per source account.
    /// Caps sequence churn and prevents a single account from deadlocking.
    pub max_concurrent_per_account: usize,
    /// Maximum number of submission attempts for a single transaction before
    /// it is treated as terminally failed.
    pub max_submission_attempts: u32,
    /// Base backoff (in milliseconds) for transient submission errors.
    pub retry_base_backoff_ms: u64,
    /// Maximum backoff (in milliseconds) for transient submission errors.
    pub retry_max_backoff_ms: u64,
    /// Overall timeout (in seconds) for a single submission attempt against
    /// Horizon. A timeout does not imply failure — the transaction may still
    /// be included, so confirmation is checked idempotently.
    pub submission_timeout_secs: u64,
    /// How long (in seconds) a sequence lease is held before it is considered
    /// stale and may be reclaimed by another replica.
    pub sequence_lease_ttl_secs: u64,
}

impl Default for StellarSettlementConfig {
    fn default() -> Self {
        Self {
            source_account: None,
            channel_accounts: Vec::new(),
            max_concurrent_per_account: 4,
            max_submission_attempts: 5,
            retry_base_backoff_ms: 250,
            retry_max_backoff_ms: 8000,
            submission_timeout_secs: 30,
            sequence_lease_ttl_secs: 60,
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
    // LP adapter onboarding, credentialing, and certification
    pub lp_adapter: LpAdapterConfig,
    // Stellar settlement submission (sequence management + retry)
    pub stellar_settlement: StellarSettlementConfig,
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

        // LP adapter configuration. No secret material is read here: credentials
        // are resolved lazily through SecretsManager so rotation does not require
        // a restart and secrets never enter the Config struct or logs.
        let lp_adapter = LpAdapterConfig {
            manifest_dir: env::var("LP_MANIFEST_DIR")
                .unwrap_or_else(|_| "./config/lp-adapters".to_string()),
            credential_source: LpCredentialSource::from_str(
                &env::var("LP_CREDENTIAL_SOURCE").unwrap_or_else(|_| "environment".to_string()),
            ),
            credential_rotation_interval_secs: env::var("LP_CREDENTIAL_ROTATION_INTERVAL_SECS")
                .unwrap_or_else(|_| "300".to_string())
                .parse()?,
            credential_rotation_grace_secs: env::var("LP_CREDENTIAL_ROTATION_GRACE_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()?,
            require_certification: env::var("LP_REQUIRE_CERTIFICATION")
                .unwrap_or_else(|_| "true".to_string())
                .parse()?,
            certification_report_dir: env::var("LP_CERTIFICATION_REPORT_DIR")
                .unwrap_or_else(|_| "./certification-reports".to_string()),
        };

        // Stellar settlement submission configuration. Channel accounts are
        // public keys only; signing keys are resolved via SecretsManager.
        let channel_accounts = env::var("STELLAR_CHANNEL_ACCOUNTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect::<Vec<_>>();

        let stellar_settlement = StellarSettlementConfig {
            source_account: env::var("STELLAR_SETTLEMENT_SOURCE_ACCOUNT").ok(),
            channel_accounts,
            max_concurrent_per_account: env::var("STELLAR_MAX_CONCURRENT_PER_ACCOUNT")
                .unwrap_or_else(|_| "4".to_string())
                .parse()?,
            max_submission_attempts: env::var("STELLAR_MAX_SUBMISSION_ATTEMPTS")
                .unwrap_or_else(|_| "5".to_string())
                .parse()?,
            retry_base_backoff_ms: env::var("STELLAR_RETRY_BASE_BACKOFF_MS")
                .unwrap_or_else(|_| "250".to_string())
                .parse()?,
            retry_max_backoff_ms: env::var("STELLAR_RETRY_MAX_BACKOFF_MS")
                .unwrap_or_else(|_| "8000".to_string())
                .parse()?,
            submission_timeout_secs: env::var("STELLAR_SUBMISSION_TIMEOUT_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()?,
            sequence_lease_ttl_secs: env::var("STELLAR_SEQUENCE_LEASE_TTL_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()?,
        };

        if stellar_settlement.retry_base_backoff_ms > stellar_settlement.retry_max_backoff_ms {
            anyhow::bail!(
                "STELLAR_RETRY_BASE_BACKOFF_MS ({}) cannot exceed STELLAR_RETRY_MAX_BACKOFF_MS ({})",
                stellar_settlement.retry_base_backoff_ms,
                stellar_settlement.retry_max_backoff_ms
            );
        }

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
            lp_adapter,
            stellar_settlement,
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