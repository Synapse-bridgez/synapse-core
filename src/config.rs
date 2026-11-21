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
    /// How long (in milliseconds) pool statistics may be served from cache
    /// before the health endpoint re-queries the database. Default: 1000ms.
    pub pool_stats_cache_ttl_ms: u64,
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
                .unwrap_or_else(|| env::var("DATABASE_URL").unwrap_or_defa

/* … truncated 5846 chars — edit only what you need near the top … */
