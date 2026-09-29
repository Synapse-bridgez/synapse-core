use crate::{error::AppError, AppState};
use axum::{extract::State, Json};
use serde_json::{json, Value};

/// Read-only, secret-free snapshot used by environment-parity tooling.
pub async fn export_config(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let environment = match env_value("APP_ENV", "development")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "prod" | "production" => "production",
        "staging" => "staging",
        _ => "development",
    };
    let (log_format, log_level, rate_limit, whitelist_rate_limit, read_timeout, write_timeout) =
        match environment {
            "production" => ("json", "warn", "100", "1000", "5", "10"),
            "staging" => ("json", "info", "500", "5000", "10", "20"),
            _ => ("text", "debug", "10000", "100000", "30", "60"),
        };

    let feature_flags = state.feature_flags.get_all_flags().await?;
    Ok(Json(json!({
        "environment": environment,
        "settings": {
            "logging": {
                "format": env_value("LOG_FORMAT", log_format),
                "level": env_value("RUST_LOG", log_level),
            },
            "rate_limits": {
                "default_per_minute": env_value("DEFAULT_RATE_LIMIT", rate_limit),
                "whitelist_per_minute": env_value("WHITELIST_RATE_LIMIT", whitelist_rate_limit),
            },
            "database": {
                "min_connections": env_value("DB_MIN_CONNECTIONS", "5"),
                "max_connections": env_value("DB_MAX_CONNECTIONS", "50"),
                "statement_timeout_ms": env_value("DB_STATEMENT_TIMEOUT_MS", "30000"),
                "idle_timeout_secs": env_value("DB_IDLE_TIMEOUT_SECS", "600"),
                "long_running_statement_timeout_ms": env_value("DB_LONG_RUNNING_STATEMENT_TIMEOUT_MS", "300000"),
                "read_timeout_secs": env_value("DB_TIMEOUT_READ_SECS", read_timeout),
                "write_timeout_secs": env_value("DB_TIMEOUT_WRITE_SECS", write_timeout),
            },
            "processor": {
                "workers": env_value("PROCESSOR_WORKERS", "4"),
                "batch_size": env_value("PROCESSOR_BATCH_SIZE", "50"),
                "poll_interval_ms": env_value("PROCESSOR_POLL_INTERVAL_MS", "1000"),
                "min_batch": env_value("PROCESSOR_MIN_BATCH", "10"),
                "max_batch": env_value("PROCESSOR_MAX_BATCH", "500"),
            },
            "queue": {
                "max_pending": env_value("MAX_PENDING_QUEUE", "10000"),
            },
            "background_tasks": {
                "settlement": { "max_concurrent": 1, "timeout_secs": 120 },
                "webhook": { "max_concurrent": 10, "timeout_secs": 60 },
            },
        },
        "feature_flags": feature_flags,
    })))
}

fn env_value(name: &str, fallback: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| fallback.to_owned())
}
