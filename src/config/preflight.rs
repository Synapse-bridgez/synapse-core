//! Startup configuration validation and deployment preflight checks (#1517).
//!
//! [`validate_env`] runs every static check (presence, type/range, cross-field
//! consistency) and accumulates all problems into one [`PreflightReport`]
//! instead of stopping at the first. `Config::load` calls it so the service
//! fails fast at startup with the whole report; the `preflight` CLI command
//! additionally runs [`check_dependencies`] (bounded-timeout reachability of
//! PostgreSQL, Redis and Horizon) and [`pending_migrations`] (drift).
//!
//! Every check is read-only. Secret values are never rendered - only whether
//! they are present - and non-secret values are echoed only for keys listed in
//! the checks below.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// Variables whose values must never appear in output.
pub const SECRET_VARS: &[&str] = &[
    "DATABASE_URL",
    "DATABASE_REPLICA_URL",
    "BACKUP_DATABASE_URL",
    "DATABASE_URL_TEMPLATE",
    "ANCHOR_WEBHOOK_SECRET",
    "BACKUP_ENCRYPTION_KEY",
    "REDIS_URL",
    "VAULT_ROLE_ID",
    "VAULT_SECRET_ID",
];

/// Default per-dependency timeout for reachability checks.
pub const DEFAULT_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Stable check name, e.g. `required`, `range`, `consistency`.
    pub check: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightReport {
    pub problems: Vec<Problem>,
}

impl PreflightReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }

    pub fn push(&mut self, check: &'static str, message: impl Into<String>) {
        self.problems.push(Problem {
            check,
            message: message.into(),
        });
    }

    pub fn extend(&mut self, other: PreflightReport) {
        self.problems.extend(other.problems);
    }

    /// `Ok(())` when clean, otherwise an error carrying the rendered report.
    pub fn into_result(self) -> anyhow::Result<()> {
        if self.is_ok() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("{self}"))
        }
    }
}

impl fmt::Display for PreflightReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_ok() {
            return write!(f, "configuration preflight passed");
        }
        writeln!(
            f,
            "configuration preflight failed with {} problem(s):",
            self.problems.len()
        )?;
        for p in &self.problems {
            writeln!(f, "  - [{}] {}", p.check, p.message)?;
        }
        Ok(())
    }
}

/// Static validation over an environment lookup (`|k| std::env::var(k).ok()`
/// in production; a map in tests). Returns every problem found.
pub fn validate_env(get: impl Fn(&str) -> Option<String>) -> PreflightReport {
    let mut r = PreflightReport::default();
    let present = |k: &str| get(k).is_some_and(|v| !v.trim().is_empty());

    // --- presence --------------------------------------------------------
    let use_vault = present("VAULT_ROLE_ID") && present("VAULT_SECRET_ID");
    if present("VAULT_ROLE_ID") != present("VAULT_SECRET_ID") {
        r.push(
            "consistency",
            "VAULT_ROLE_ID and VAULT_SECRET_ID must be set together",
        );
    }
    let mut required = vec!["STELLAR_HORIZON_URL"];
    if !use_vault {
        required.extend(["DATABASE_URL", "ANCHOR_WEBHOOK_SECRET"]);
    }
    for k in required {
        if !present(k) {
            r.push("required", format!("{k} is not set"));
        }
    }

    // --- format (secret values are never echoed) -------------------------
    let check_scheme = |r: &mut PreflightReport, k: &str, schemes: &[&str]| {
        if let Some(v) = get(k).filter(|v| !v.trim().is_empty()) {
            if !schemes.iter().any(|s| v.starts_with(s)) {
                r.push(
                    "format",
                    format!("{k} must start with one of {}", schemes.join(", ")),
                );
            }
        }
    };
    check_scheme(&mut r, "DATABASE_URL", &["postgres://", "postgresql://"]);
    check_scheme(
        &mut r,
        "DATABASE_REPLICA_URL",
        &["postgres://", "postgresql://"],
    );
    check_scheme(
        &mut r,
        "BACKUP_DATABASE_URL",
        &["postgres://", "postgresql://"],
    );
    check_scheme(&mut r, "REDIS_URL", &["redis://", "rediss://"]);
    check_scheme(&mut r, "STELLAR_HORIZON_URL", &["http://", "https://"]);
    check_scheme(&mut r, "OTLP_ENDPOINT", &["http://", "https://"]);

    if let Some(v) = get("LOG_FORMAT") {
        if !matches!(v.trim().to_ascii_lowercase().as_str(), "text" | "json") {
            r.push(
                "format",
                format!("LOG_FORMAT must be text or json (got {v:?})"),
            );
        }
    }
    if let Some(v) = get("APP_ENV") {
        let v = v.trim().to_ascii_lowercase();
        if !matches!(
            v.as_str(),
            "development" | "dev" | "staging" | "production" | "prod"
        ) {
            r.push(
                "format",
                format!("APP_ENV must be development, staging or production (got {v:?})"),
            );
        }
    }
    if let Some(v) = get("AUTO_MIGRATE") {
        if v.parse::<bool>().is_err() {
            r.push(
                "type",
                format!("AUTO_MIGRATE must be true or false (got {v:?})"),
            );
        }
    }

    // --- type and range --------------------------------------------------
    fn num<T: FromStr + PartialOrd + fmt::Display + Copy>(
        r: &mut PreflightReport,
        get: &impl Fn(&str) -> Option<String>,
        k: &str,
        min: T,
        max: T,
    ) -> Option<T> {
        let raw = get(k)?;
        match raw.trim().parse::<T>() {
            Ok(v) if v < min || v > max => {
                r.push("range", format!("{k}={v} is outside [{min}, {max}]"));
                None
            }
            Ok(v) => Some(v),
            Err(_) => {
                r.push("type", format!("{k} must be a number (got {raw:?})"));
                None
            }
        }
    }

    num::<u16>(&mut r, &get, "SERVER_PORT", 1, u16::MAX);
    num::<u32>(&mut r, &get, "DEFAULT_RATE_LIMIT", 1, u32::MAX);
    num::<u32>(&mut r, &get, "WHITELIST_RATE_LIMIT", 1, u32::MAX);
    num::<usize>(&mut r, &get, "TRUSTED_PROXY_DEPTH", 0, 16);
    num::<u64>(&mut r, &get, "MAX_PENDING_QUEUE", 1, u64::MAX);
    num::<usize>(&mut r, &get, "PROCESSOR_WORKERS", 1, 1024);
    num::<u64>(&mut r, &get, "PROCESSOR_POLL_INTERVAL_MS", 1, 3_600_000);
    num::<f64>(&mut r, &get, "PROCESSOR_SCALING_FACTOR", 0.0, 100.0);
    num::<u64>(&mut r, &get, "SLOW_QUERY_THRESHOLD_MS", 1, 3_600_000);
    num::<u64>(&mut r, &get, "DB_STATEMENT_TIMEOUT_MS", 1, 86_400_000);
    num::<u64>(&mut r, &get, "DB_IDLE_TIMEOUT_SECS", 1, 86_400);
    num::<u32>(&mut r, &get, "PARTITION_LOOKAHEAD_MONTHS", 1, 120);

    // --- cross-field consistency ------------------------------------------
    let mut ordered = |a: &str, b: &str, av: Option<f64>, bv: Option<f64>| {
        if let (Some(x), Some(y)) = (av, bv) {
            if x > y {
                r.push("consistency", format!("{a} ({x}) must be <= {b} ({y})"));
            }
        }
    };
    let as_f = |v: Option<u64>| v.map(|v| v as f64);

    let mut tmp = PreflightReport::default();
    let min_conn = num::<u32>(&mut tmp, &get, "DB_MIN_CONNECTIONS", 0, 10_000);
    let max_conn = num::<u32>(&mut tmp, &get, "DB_MAX_CONNECTIONS", 1, 10_000);
    let read = num::<u64>(&mut tmp, &get, "DB_TIMEOUT_READ_SECS", 1, 3_600);
    let write = num::<u64>(&mut tmp, &get, "DB_TIMEOUT_WRITE_SECS", 1, 3_600);
    let admin = num::<u64>(&mut tmp, &get, "DB_TIMEOUT_ADMIN_SECS", 1, 86_400);
    let min_batch = num::<u32>(&mut tmp, &get, "PROCESSOR_MIN_BATCH", 1, 1_000_000);
    let max_batch = num::<u32>(&mut tmp, &get, "PROCESSOR_MAX_BATCH", 1, 1_000_000);
    let batch = num::<u32>(&mut tmp, &get, "PROCESSOR_BATCH_SIZE", 1, 1_000_000);
    let la_min = num::<u32>(&mut tmp, &get, "PARTITION_LOOKAHEAD_MIN", 1, 120);
    let la_max = num::<u32>(&mut tmp, &get, "PARTITION_LOOKAHEAD_MAX", 1, 120);
    let settle_min = num::<usize>(&mut tmp, &get, "SETTLEMENT_MIN_TX_COUNT", 1, 1_000_000);
    let settle_max = num::<usize>(&mut tmp, &get, "SETTLEMENT_MAX_BATCH_SIZE", 1, 1_000_000);

    ordered(
        "DB_MIN_CONNECTIONS",
        "DB_MAX_CONNECTIONS",
        min_conn.map(f64::from),
        max_conn.map(f64::from),
    );
    ordered(
        "DB_TIMEOUT_READ_SECS",
        "DB_TIMEOUT_WRITE_SECS",
        as_f(read),
        as_f(write),
    );
    ordered(
        "DB_TIMEOUT_WRITE_SECS",
        "DB_TIMEOUT_ADMIN_SECS",
        as_f(write),
        as_f(admin),
    );
    ordered(
        "PROCESSOR_MIN_BATCH",
        "PROCESSOR_MAX_BATCH",
        min_batch.map(f64::from),
        max_batch.map(f64::from),
    );
    ordered(
        "PROCESSOR_MIN_BATCH",
        "PROCESSOR_BATCH_SIZE",
        min_batch.map(f64::from),
        batch.map(f64::from),
    );
    ordered(
        "PROCESSOR_BATCH_SIZE",
        "PROCESSOR_MAX_BATCH",
        batch.map(f64::from),
        max_batch.map(f64::from),
    );
    ordered(
        "PARTITION_LOOKAHEAD_MIN",
        "PARTITION_LOOKAHEAD_MAX",
        la_min.map(f64::from),
        la_max.map(f64::from),
    );
    ordered(
        "SETTLEMENT_MIN_TX_COUNT",
        "SETTLEMENT_MAX_BATCH_SIZE",
        settle_min.map(|v| v as f64),
        settle_max.map(|v| v as f64),
    );
    r.extend(tmp);

    // --- dependencies between settings -------------------------------------
    // With Vault, the database URL comes from DATABASE_URL_TEMPLATE and falls
    // back to DATABASE_URL; with neither, Config::load would use an empty URL.
    if use_vault && !present("DATABASE_URL_TEMPLATE") && !present("DATABASE_URL") {
        r.push(
            "required",
            "Vault is enabled but neither DATABASE_URL_TEMPLATE nor DATABASE_URL is set",
        );
    }
    if let Some(t) = get("DATABASE_URL_TEMPLATE").filter(|t| !t.trim().is_empty()) {
        if !t.contains("{password}") {
            r.push("format", "DATABASE_URL_TEMPLATE must contain {password}");
        }
    }

    r
}

/// Present/absent summary of secret variables, safe to print.
pub fn secret_presence(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, bool)> {
    SECRET_VARS
        .iter()
        .map(|k| (*k, get(k).is_some_and(|v| !v.trim().is_empty())))
        .collect()
}

/// Bounded-timeout reachability of PostgreSQL, Redis and Horizon. Read-only:
/// one `SELECT 1`, one `PING`, one `GET` of the Horizon root.
pub async fn check_dependencies(
    config: &crate::config::Config,
    timeout: Duration,
) -> PreflightReport {
    let mut r = PreflightReport::default();

    let pg = async {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(timeout)
            .connect(&config.database_url)
            .await?;
        sqlx::query("SELECT 1").execute(&pool).await?;
        pool.close().await;
        Ok::<_, sqlx::Error>(())
    };
    match tokio::time::timeout(timeout, pg).await {
        Ok(Ok(())) => {}
        // sqlx errors can embed the connection string; report the kind only.
        Ok(Err(e)) => r.push("reachability", format!("PostgreSQL: {}", error_kind(&e))),
        Err(_) => r.push(
            "reachability",
            format!("PostgreSQL: timed out after {timeout:?}"),
        ),
    }

    let redis_check = async {
        let client = redis::Client::open(config.redis_url.as_str())?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        redis::cmd("PING")
            .query_async::<_, String>(&mut conn)
            .await?;
        Ok::<_, redis::RedisError>(())
    };
    match tokio::time::timeout(timeout, redis_check).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => r.push("reachability", format!("Redis: {:?}", e.kind())),
        Err(_) => r.push(
            "reachability",
            format!("Redis: timed out after {timeout:?}"),
        ),
    }

    let horizon = async {
        reqwest::Client::builder()
            .timeout(timeout)
            .build()?
            .get(&config.stellar_horizon_url)
            .send()
            .await?
            .error_for_status()
    };
    match tokio::time::timeout(timeout, horizon).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => r.push(
            "reachability",
            format!(
                "Horizon: request failed{}",
                e.status().map(|s| format!(" with {s}")).unwrap_or_default()
            ),
        ),
        Err(_) => r.push(
            "reachability",
            format!("Horizon: timed out after {timeout:?}"),
        ),
    }

    r
}

fn error_kind(e: &sqlx::Error) -> &'static str {
    match e {
        sqlx::Error::Configuration(_) => "invalid configuration",
        sqlx::Error::Database(_) => "database rejected the connection",
        sqlx::Error::Io(_) => "connection failed (I/O)",
        sqlx::Error::Tls(_) => "TLS error",
        sqlx::Error::PoolTimedOut => "connection timed out",
        _ => "connection failed",
    }
}

/// Versions present in `migrator` but not successfully applied to the
/// database. Read-only (`SELECT` on `_sqlx_migrations`); a missing table means
/// every migration is pending.
pub async fn pending_migrations(
    pool: &sqlx::PgPool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<Vec<i64>, sqlx::Error> {
    let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass('_sqlx_migrations')::text")
        .fetch_one(pool)
        .await?;
    let applied: Vec<i64> = if exists.is_some() {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(pool)
            .await?
    } else {
        Vec::new()
    };
    Ok(migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .map(|m| m.version)
        .collect())
}

/// Whether startup may apply migrations itself (`AUTO_MIGRATE`, default true).
pub fn auto_migrate_enabled() -> bool {
    std::env::var("AUTO_MIGRATE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| m.get(k).cloned()
    }

    const VALID: &[(&str, &str)] = &[
        ("DATABASE_URL", "postgres://user:hunter2@db/app"),
        ("ANCHOR_WEBHOOK_SECRET", "s3cr3t-value"),
        ("STELLAR_HORIZON_URL", "https://horizon-testnet.stellar.org"),
        ("REDIS_URL", "redis://localhost:6379"),
    ];

    fn with(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        let mut v: Vec<_> = VALID.to_vec();
        for (k, val) in extra {
            v.retain(|(ek, _)| ek != k);
            v.push((k, val));
        }
        v
    }

    #[test]
    fn valid_minimal_config_passes() {
        assert_eq!(validate_env(env(VALID)), PreflightReport::default());
    }

    #[test]
    fn reports_every_problem_not_just_the_first() {
        let report = validate_env(env(&[
            ("SERVER_PORT", "not-a-port"),
            ("DB_MIN_CONNECTIONS", "50"),
            ("DB_MAX_CONNECTIONS", "10"),
            ("LOG_FORMAT", "xml"),
        ]));
        let checks: Vec<_> = report.problems.iter().map(|p| p.check).collect();
        // 3 missing required + type + consistency + format
        assert_eq!(report.problems.len(), 6, "{report}");
        assert_eq!(checks.iter().filter(|c| **c == "required").count(), 3);
        assert!(checks.contains(&"type"));
        assert!(checks.contains(&"consistency"));
        assert!(checks.contains(&"format"));
    }

    /// (extra env vars, expected problem substrings)
    type Case = (
        &'static [(&'static str, &'static str)],
        &'static [&'static str],
    );

    #[test]
    fn table_driven_malformed_combinations() {
        let cases: &[Case] = &[
            (
                &[
                    ("DB_TIMEOUT_READ_SECS", "30"),
                    ("DB_TIMEOUT_WRITE_SECS", "10"),
                ],
                &["DB_TIMEOUT_READ_SECS (30)"],
            ),
            (
                &[
                    ("DB_TIMEOUT_WRITE_SECS", "90"),
                    ("DB_TIMEOUT_ADMIN_SECS", "60"),
                ],
                &["DB_TIMEOUT_WRITE_SECS (90)"],
            ),
            (
                &[
                    ("PROCESSOR_MIN_BATCH", "100"),
                    ("PROCESSOR_MAX_BATCH", "10"),
                ],
                &["PROCESSOR_MIN_BATCH (100)"],
            ),
            (
                &[
                    ("PARTITION_LOOKAHEAD_MIN", "6"),
                    ("PARTITION_LOOKAHEAD_MAX", "2"),
                ],
                &["PARTITION_LOOKAHEAD_MIN (6)"],
            ),
            (&[("SERVER_PORT", "0")], &["SERVER_PORT=0 is outside"]),
            (
                &[("TRUSTED_PROXY_DEPTH", "-1")],
                &["TRUSTED_PROXY_DEPTH must be a number"],
            ),
            (
                &[("REDIS_URL", "localhost:6379")],
                &["REDIS_URL must start with"],
            ),
            (
                &[("DATABASE_URL", "mysql://x")],
                &["DATABASE_URL must start with"],
            ),
            (
                &[("VAULT_ROLE_ID", "r")],
                &["VAULT_ROLE_ID and VAULT_SECRET_ID"],
            ),
            (&[("AUTO_MIGRATE", "maybe")], &["AUTO_MIGRATE must be"]),
            (
                &[("DATABASE_URL_TEMPLATE", "postgres://u@h/db")],
                &["must contain {password}"],
            ),
            (&[("APP_ENV", "prod-eu")], &["APP_ENV must be"]),
        ];
        for (extra, expected) in cases {
            let vars = with(extra);
            let report = validate_env(env(&vars));
            assert_eq!(report.problems.len(), expected.len(), "{extra:?}: {report}");
            for e in *expected {
                assert!(
                    report.problems.iter().any(|p| p.message.contains(e)),
                    "{extra:?}: expected {e:?} in {report}"
                );
            }
        }
    }

    #[test]
    fn vault_replaces_database_url_and_anchor_secret() {
        let vault_only = [
            ("VAULT_ROLE_ID", "r"),
            ("VAULT_SECRET_ID", "s"),
            ("STELLAR_HORIZON_URL", "https://h"),
        ];
        let report = validate_env(env(&vault_only));
        assert_eq!(report.problems.len(), 1, "{report}");
        assert!(report.problems[0].message.contains("DATABASE_URL_TEMPLATE"));

        let mut with_template = vault_only.to_vec();
        with_template.push(("DATABASE_URL_TEMPLATE", "postgres://u:{password}@h/db"));
        assert!(validate_env(env(&with_template)).is_ok());
    }

    #[test]
    fn secrets_never_appear_in_output() {
        let report = validate_env(env(&[
            ("DATABASE_URL", "mysql://user:hunter2@db/app"),
            ("REDIS_URL", "tcp://:redispass@cache"),
            ("ANCHOR_WEBHOOK_SECRET", "s3cr3t-value"),
            ("STELLAR_HORIZON_URL", "ftp://h"),
        ]));
        let out = report.to_string();
        assert_eq!(report.problems.len(), 3, "{out}");
        for secret in ["hunter2", "redispass", "s3cr3t-value"] {
            assert!(!out.contains(secret), "leaked {secret} in {out}");
        }
        let presence = secret_presence(env(VALID));
        assert!(presence.contains(&("DATABASE_URL", true)));
        assert!(presence.contains(&("BACKUP_ENCRYPTION_KEY", false)));
    }

    #[test]
    fn report_renders_all_problems() {
        let mut r = PreflightReport::default();
        r.push("required", "A is not set");
        r.push("range", "B=0 is outside [1, 2]");
        let s = r.clone().into_result().unwrap_err().to_string();
        assert!(s.contains("2 problem(s)") && s.contains("A is not set") && s.contains("B=0"));
    }
}
