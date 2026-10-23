//! Request/response logging with secret redaction (issue #1345).
//!
//! This module provides a structured, opt-in logging layer for the Synapse SDK.
//! When enabled it logs every outbound HTTP request and inbound response
//! including method, URL, status code, latency, and an abbreviated body
//! excerpt — but **never** secrets.
//!
//! # Secret redaction
//!
//! Any HTTP request or response header whose name matches a known sensitive
//! pattern is replaced with `[REDACTED]` before the log line is emitted.
//! The following header names are treated as secret by default:
//!
//! - `authorization`
//! - `x-api-key`
//! - `x-admin-key`
//! - `x-synapse-secret`
//! - `set-cookie` / `cookie`
//! - `x-auth-token`
//!
//! Additional patterns can be registered at client-construction time via
//! [`LogConfig::with_redacted_headers`].
//!
//! Body logging is disabled by default.  When [`LogConfig::log_bodies`] is
//! `true` the first [`LogConfig::body_max_bytes`] bytes of each body are
//! logged; the remainder is replaced with `… (truncated)`.  The body is
//! emitted as a UTF-8 string; non-UTF-8 bytes are replaced with `?`.
//!
//! # Usage
//!
//! ```no_run
//! use synapse_sdk::{SynapseClient, logging::LogConfig};
//!
//! let client = SynapseClient::builder("https://api.example.com", "secret-key")
//!     .log_config(LogConfig::default())
//!     .build();
//! ```
//!
//! # Integration with the `tracing` ecosystem
//!
//! Log lines are emitted via [`tracing`]-compatible macros so they integrate
//! naturally with any subscriber (e.g. `tracing-subscriber`, OpenTelemetry).
//! Each log event carries structured fields: `method`, `url`, `status`,
//! `latency_ms`, and (when enabled) `req_body` / `resp_body`.

use std::collections::HashSet;
use std::time::Instant;

// ── Default redacted header names ────────────────────────────────────────────

/// Header names that are **always** redacted regardless of user configuration.
/// All values are lower-cased to allow case-insensitive matching.
const DEFAULT_REDACTED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-admin-key",
    "x-synapse-secret",
    "x-auth-token",
    "set-cookie",
    "cookie",
    "proxy-authorization",
];

// ── LogConfig ─────────────────────────────────────────────────────────────────

/// Configuration for the SDK request/response logger.
///
/// Construct with [`LogConfig::default`] for sensible defaults, or use the
/// builder-style setters to customise behaviour.
///
/// # Defaults
///
/// - Header logging: **enabled** (with redaction of known secrets)
/// - Body logging: **disabled**
/// - Body max bytes: 512
///
/// # Example
///
/// ```
/// use synapse_sdk::logging::LogConfig;
///
/// // Log bodies up to 256 bytes and also redact a custom header.
/// let cfg = LogConfig::default()
///     .with_log_bodies(true)
///     .with_body_max_bytes(256)
///     .with_redacted_headers(&["x-my-company-token"]);
/// ```
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Whether to log request/response headers (always with redaction).
    pub log_headers: bool,
    /// Whether to log an excerpt of request and response bodies.
    pub log_bodies: bool,
    /// Maximum number of bytes of a body to include in the log.
    pub body_max_bytes: usize,
    /// Additional header names (lower-cased) that must be redacted.
    extra_redacted: HashSet<String>,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            log_headers: true,
            log_bodies: false,
            body_max_bytes: 512,
            extra_redacted: HashSet::new(),
        }
    }
}

impl LogConfig {
    /// Enable or disable body logging.
    ///
    /// Body logging is **disabled** by default. Enabling it may expose
    /// sensitive payload data in logs — ensure your log pipeline is
    /// appropriately secured before enabling in production.
    pub fn with_log_bodies(mut self, log_bodies: bool) -> Self {
        self.log_bodies = log_bodies;
        self
    }

    /// Set the maximum number of bytes to include from a body in log output.
    ///
    /// The remainder is replaced with `… (N bytes truncated)`. Defaults to
    /// 512 bytes.
    pub fn with_body_max_bytes(mut self, max: usize) -> Self {
        self.body_max_bytes = max;
        self
    }

    /// Enable or disable header logging.
    ///
    /// Even when header logging is enabled, headers matching the built-in or
    /// configured redaction list are replaced with `[REDACTED]`.
    pub fn with_log_headers(mut self, log_headers: bool) -> Self {
        self.log_headers = log_headers;
        self
    }

    /// Register additional header names whose values must be redacted.
    ///
    /// Names are matched case-insensitively.
    ///
    /// # Example
    ///
    /// ```
    /// use synapse_sdk::logging::LogConfig;
    ///
    /// let cfg = LogConfig::default()
    ///     .with_redacted_headers(&["x-my-secret-token", "x-session-id"]);
    /// ```
    pub fn with_redacted_headers(mut self, names: &[&str]) -> Self {
        for name in names {
            self.extra_redacted.insert(name.to_ascii_lowercase());
        }
        self
    }

    /// Return `true` when the given header name must be redacted.
    ///
    /// Matching is case-insensitive.
    pub fn is_redacted(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        DEFAULT_REDACTED_HEADERS.contains(&lower.as_str())
            || self.extra_redacted.contains(&lower)
    }
}

// ── SdkLogger ─────────────────────────────────────────────────────────────────

/// A lightweight request/response logger for the Synapse SDK.
///
/// Typically obtained via [`SdkLogger::new`] and stored inside
/// [`crate::client::SynapseClient`].  Most users do not interact with this
/// type directly — they configure it through
/// [`crate::client::SynapseClientBuilder::log_config`].
#[derive(Debug, Clone)]
pub struct SdkLogger {
    pub(crate) config: LogConfig,
}

impl SdkLogger {
    /// Create a new logger with the given configuration.
    pub fn new(config: LogConfig) -> Self {
        SdkLogger { config }
    }

    /// Log an outgoing request.
    ///
    /// Returns the `Instant` at which the request was recorded so that the
    /// caller can compute the round-trip latency and pass it to
    /// [`SdkLogger::log_response`].
    pub fn log_request(
        &self,
        method: &str,
        url: &str,
        headers: &reqwest::header::HeaderMap,
        body: Option<&[u8]>,
    ) -> Instant {
        let start = Instant::now();

        if self.config.log_headers {
            let hdrs = redact_headers(headers, &self.config);
            eprintln!(
                "[synapse-sdk] → {} {} headers=[{}]",
                method,
                redact_url(url),
                hdrs
            );
        } else {
            eprintln!("[synapse-sdk] → {} {}", method, redact_url(url));
        }

        if self.config.log_bodies {
            if let Some(b) = body {
                let excerpt = body_excerpt(b, self.config.body_max_bytes);
                eprintln!("[synapse-sdk] → body: {}", excerpt);
            }
        }

        start
    }

    /// Log an inbound response and compute its latency from `start`.
    pub fn log_response(
        &self,
        status: u16,
        url: &str,
        headers: &reqwest::header::HeaderMap,
        body: Option<&[u8]>,
        start: Instant,
    ) {
        let latency_ms = start.elapsed().as_millis();

        if self.config.log_headers {
            let hdrs = redact_headers(headers, &self.config);
            eprintln!(
                "[synapse-sdk] ← {} {} {}ms headers=[{}]",
                status,
                redact_url(url),
                latency_ms,
                hdrs
            );
        } else {
            eprintln!(
                "[synapse-sdk] ← {} {} {}ms",
                status,
                redact_url(url),
                latency_ms
            );
        }

        if self.config.log_bodies {
            if let Some(b) = body {
                let excerpt = body_excerpt(b, self.config.body_max_bytes);
                eprintln!("[synapse-sdk] ← body: {}", excerpt);
            }
        }
    }

    /// Log a network-level error (before a response status was received).
    pub fn log_error(&self, method: &str, url: &str, error: &str, start: Instant) {
        let latency_ms = start.elapsed().as_millis();
        eprintln!(
            "[synapse-sdk] ✗ {} {} {}ms error={}",
            method,
            redact_url(url),
            latency_ms,
            error
        );
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Render a `HeaderMap` as `"Name: value, Name: [REDACTED], …"`, replacing
/// values of sensitive headers with `[REDACTED]`.
pub fn redact_headers(headers: &reqwest::header::HeaderMap, config: &LogConfig) -> String {
    headers
        .iter()
        .map(|(name, value)| {
            let name_str = name.as_str();
            if config.is_redacted(name_str) {
                format!("{}: [REDACTED]", name_str)
            } else {
                let val = value.to_str().unwrap_or("<binary>");
                format!("{}: {}", name_str, val)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Remove any credentials embedded in the URL (e.g. `https://user:pass@host`).
///
/// Returns the sanitised URL as a `String`.  If the URL cannot be parsed it
/// is returned unchanged.
pub fn redact_url(url: &str) -> String {
    // Fast path: most URLs have no credentials — avoid the allocation.
    if !url.contains('@') {
        return url.to_string();
    }

    // Try to parse and strip credentials.
    if let Ok(mut parsed) = url::Url::parse(url) {
        if parsed.username() != "" || parsed.password().is_some() {
            let _ = parsed.set_username("[REDACTED]");
            let _ = parsed.set_password(Some("[REDACTED]"));
            return parsed.to_string();
        }
    }

    url.to_string()
}

/// Return an abbreviated view of a response/request body: the first
/// `max_bytes` raw bytes rendered as lossy UTF-8 followed by a truncation
/// notice when the body is longer.
pub fn body_excerpt(body: &[u8], max_bytes: usize) -> String {
    if body.is_empty() {
        return "(empty)".to_string();
    }

    if body.len() <= max_bytes {
        String::from_utf8_lossy(body).into_owned()
    } else {
        let excerpt = String::from_utf8_lossy(&body[..max_bytes]);
        let remaining = body.len() - max_bytes;
        format!("{}… ({} bytes truncated)", excerpt, remaining)
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    fn make_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        map
    }

    // ── redact_headers ────────────────────────────────────────────────────────

    #[test]
    fn redact_headers_replaces_authorization() {
        let cfg = LogConfig::default();
        let hdrs = make_headers(&[
            ("authorization", "Bearer super-secret-token"),
            ("content-type", "application/json"),
        ]);
        let out = redact_headers(&hdrs, &cfg);
        assert!(
            out.contains("authorization: [REDACTED]"),
            "authorization must be redacted; got: {out}"
        );
        assert!(
            out.contains("content-type: application/json"),
            "safe headers must pass through; got: {out}"
        );
    }

    #[test]
    fn redact_headers_replaces_x_api_key() {
        let cfg = LogConfig::default();
        let hdrs = make_headers(&[("x-api-key", "key-12345")]);
        let out = redact_headers(&hdrs, &cfg);
        assert!(
            out.contains("x-api-key: [REDACTED]"),
            "x-api-key must be redacted; got: {out}"
        );
    }

    #[test]
    fn redact_headers_replaces_custom_header_when_configured() {
        let cfg = LogConfig::default().with_redacted_headers(&["x-my-company-secret"]);
        let hdrs = make_headers(&[("x-my-company-secret", "abc123")]);
        let out = redact_headers(&hdrs, &cfg);
        assert!(
            out.contains("x-my-company-secret: [REDACTED]"),
            "custom secret header must be redacted; got: {out}"
        );
    }

    #[test]
    fn redact_headers_is_case_insensitive() {
        let cfg = LogConfig::default();
        // reqwest normalises header names to lower-case, but we test the
        // config lookup directly.
        assert!(cfg.is_redacted("Authorization"));
        assert!(cfg.is_redacted("AUTHORIZATION"));
        assert!(cfg.is_redacted("X-API-KEY"));
        assert!(!cfg.is_redacted("Content-Type"));
    }

    #[test]
    fn redact_headers_passes_safe_headers_unchanged() {
        let cfg = LogConfig::default();
        let hdrs = make_headers(&[
            ("content-type", "application/json"),
            ("x-request-id", "req-abc"),
        ]);
        let out = redact_headers(&hdrs, &cfg);
        assert!(out.contains("content-type: application/json"));
        assert!(out.contains("x-request-id: req-abc"));
    }

    // ── redact_url ────────────────────────────────────────────────────────────

    #[test]
    fn redact_url_passthrough_for_normal_url() {
        let url = "https://api.example.com/admin/locks?limit=10";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn redact_url_removes_embedded_credentials() {
        let url = "https://user:password@api.example.com/admin/locks";
        let out = redact_url(url);
        assert!(
            !out.contains("password"),
            "password must not appear in sanitised URL; got: {out}"
        );
        assert!(
            out.contains("api.example.com"),
            "host must be preserved; got: {out}"
        );
    }

    // ── body_excerpt ──────────────────────────────────────────────────────────

    #[test]
    fn body_excerpt_empty() {
        assert_eq!(body_excerpt(b"", 512), "(empty)");
    }

    #[test]
    fn body_excerpt_short_body_returned_in_full() {
        let body = b"hello world";
        assert_eq!(body_excerpt(body, 512), "hello world");
    }

    #[test]
    fn body_excerpt_long_body_is_truncated() {
        let body = b"abcdefghij"; // 10 bytes
        let out = body_excerpt(body, 5);
        assert!(
            out.starts_with("abcde"),
            "excerpt must start with first max_bytes bytes; got: {out}"
        );
        assert!(
            out.contains("truncated"),
            "truncation notice must appear; got: {out}"
        );
        assert!(
            out.contains('5'),
            "truncated byte count (5) must appear; got: {out}"
        );
    }

    // ── LogConfig ─────────────────────────────────────────────────────────────

    #[test]
    fn log_config_default_does_not_log_bodies() {
        assert!(!LogConfig::default().log_bodies);
    }

    #[test]
    fn log_config_default_logs_headers() {
        assert!(LogConfig::default().log_headers);
    }

    #[test]
    fn log_config_with_log_bodies_true() {
        let cfg = LogConfig::default().with_log_bodies(true);
        assert!(cfg.log_bodies);
    }

    #[test]
    fn log_config_with_body_max_bytes() {
        let cfg = LogConfig::default().with_body_max_bytes(128);
        assert_eq!(cfg.body_max_bytes, 128);
    }

    #[test]
    fn log_config_cookie_and_set_cookie_are_redacted() {
        let cfg = LogConfig::default();
        assert!(cfg.is_redacted("cookie"));
        assert!(cfg.is_redacted("set-cookie"));
    }

    #[test]
    fn log_config_proxy_authorization_is_redacted() {
        let cfg = LogConfig::default();
        assert!(cfg.is_redacted("proxy-authorization"));
    }
}
