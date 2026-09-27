use serde::Deserialize;
use thiserror::Error;

/// Errors returned by the Synapse SDK.
#[derive(Debug, Error)]
pub enum SynapseError {
    /// A structured API error returned by the server (non-2xx response).
    ///
    /// 5xx responses are transient (retryable). 4xx responses are permanent
    /// caller mistakes and are never retried.
    ///
    /// `code` is the typed, parsed form of `docs/error-catalog.md`'s `code`
    /// field when the response body carried one, letting callers match on
    /// [`ErrorCode`] instead of parsing `message` strings. It is `None` when
    /// the status is one of the named variants below (401/403/404/429,
    /// grouped there regardless of which catalog code produced them — see
    /// [`ErrorCode`] doc) or when the body carried no `code` field at all.
    #[error("API error {status}: {message}")]
    Api {
        status: u16,
        message: String,
        code: Option<ErrorCode>,
    },

    /// The requested resource was not found (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),

    /// Authentication failed or credentials are missing (HTTP 401).
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// The caller does not have permission to access the resource (HTTP 403).
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// The request has been rate-limited (HTTP 429). Back off before retrying.
    #[error("rate limit exceeded: {0}")]
    RateLimited(String),

    /// A pagination cursor was rejected as invalid or expired (HTTP 400).
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// The response body could not be decoded as the expected JSON type.
    #[error("decode error: {0}")]
    Decode(String),

    /// Raw HTTP error status — used internally by the retry layer; not
    /// produced by resource methods.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Raw HTTP error status with a server-provided `Retry-After` delay —
    /// used internally by the retry layer so backoff honors the server's
    /// own guidance instead of only client-side jitter.
    #[error("HTTP {status} (retry after {retry_after_ms}ms): {body}")]
    HttpRetryAfter {
        status: u16,
        body: String,
        retry_after_ms: u64,
    },

    /// A GraphQL-level error returned inside a 200 OK response.
    ///
    /// The server accepted and processed the request, but the GraphQL layer
    /// reported one or more errors in the `errors` array of the response body.
    /// These are distinct from transport-level failures.
    #[error("GraphQL error: {0}")]
    GraphQL(String),

    /// A network-level failure occurred before a response was received.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}

impl SynapseError {
    /// Returns `true` if this error may resolve on a subsequent attempt.
    pub fn is_transient(&self) -> bool {
        match self {
            SynapseError::Network(_) => true,
            SynapseError::HttpRetryAfter { .. } => true,
            SynapseError::Http { status, .. } | SynapseError::Api { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// The server-provided `Retry-After` delay for this error, if any.
    ///
    /// When present, the retry layer honors it instead of computing a
    /// jitter-based backoff.
    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            SynapseError::HttpRetryAfter { retry_after_ms, .. } => Some(*retry_after_ms),
            _ => None,
        }
    }
}

/// Typed form of a `docs/error-catalog.md` error code.
///
/// Every code documented in the catalog has a distinct variant here, so
/// callers can `match` on failure mode (e.g. "quota exceeded" vs "invalid
/// input") instead of parsing `message` strings.
///
/// Codes whose HTTP status already gets a dedicated, more ergonomic
/// [`SynapseError`] variant (`ERR_NOT_FOUND_001` → [`SynapseError::NotFound`],
/// `ERR_AUTH_001`/`ERR_UNAUTHORIZED_001` → [`SynapseError::Unauthorized`],
/// `ERR_AUTH_002` → [`SynapseError::Forbidden`], `ERR_RATE_LIMIT_001` →
/// [`SynapseError::RateLimited`]) still get a distinct `ErrorCode` variant
/// here too, so [`ErrorCode::from_code`] gives a complete, uniform mapping
/// regardless of which `SynapseError` shape a given response ends up in.
///
/// [`ErrorCode::Unknown`] is the graceful-degradation path for a code this
/// SDK version doesn't recognize yet (e.g. added server-side after release).
/// `sdks/rust/tests/error_catalog_sync_test.rs` cross-checks every code
/// currently in the catalog against this enum and fails the build if one
/// maps to `Unknown`, flagging the gap for the next SDK release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    Database001,
    Database002,
    Validation001,
    NotFound001,
    Internal001,
    BadRequest001,
    Auth001,
    Auth002,
    Unauthorized001,
    Transaction001,
    Transaction002,
    Transaction003,
    Transaction004,
    Transaction005,
    Transaction006,
    Webhook001,
    Webhook002,
    Settlement001,
    Settlement002,
    Settlement003,
    RateLimit001,
    Redis001,
    QueryComplexity001,
    /// A catalog code with no matching variant in this SDK version.
    Unknown(String),
}

impl ErrorCode {
    /// Parses a raw `code` string (e.g. `"ERR_TRANSACTION_004"`) from an API
    /// response into its typed form, degrading to [`ErrorCode::Unknown`]
    /// rather than panicking when the code isn't recognized.
    pub fn from_code(code: &str) -> Self {
        match code {
            "ERR_DATABASE_001" => Self::Database001,
            "ERR_DATABASE_002" => Self::Database002,
            "ERR_VALIDATION_001" => Self::Validation001,
            "ERR_NOT_FOUND_001" => Self::NotFound001,
            "ERR_INTERNAL_001" => Self::Internal001,
            "ERR_BAD_REQUEST_001" => Self::BadRequest001,
            "ERR_AUTH_001" => Self::Auth001,
            "ERR_AUTH_002" => Self::Auth002,
            "ERR_UNAUTHORIZED_001" => Self::Unauthorized001,
            "ERR_TRANSACTION_001" => Self::Transaction001,
            "ERR_TRANSACTION_002" => Self::Transaction002,
            "ERR_TRANSACTION_003" => Self::Transaction003,
            "ERR_TRANSACTION_004" => Self::Transaction004,
            "ERR_TRANSACTION_005" => Self::Transaction005,
            "ERR_TRANSACTION_006" => Self::Transaction006,
            "ERR_WEBHOOK_001" => Self::Webhook001,
            "ERR_WEBHOOK_002" => Self::Webhook002,
            "ERR_SETTLEMENT_001" => Self::Settlement001,
            "ERR_SETTLEMENT_002" => Self::Settlement002,
            "ERR_SETTLEMENT_003" => Self::Settlement003,
            "ERR_RATE_LIMIT_001" => Self::RateLimit001,
            "ERR_REDIS_001" => Self::Redis001,
            "ERR_QUERY_COMPLEXITY_001" => Self::QueryComplexity001,
            other => Self::Unknown(other.to_string()),
        }
    }
}

/// A single entry from the API's `/errors` catalog.
#[derive(Debug, Clone, Deserialize)]
pub struct CatalogEntry {
    pub code: String,
    pub http_status: u16,
    pub description: String,
}

/// Response shape of `GET /errors`.
#[derive(Debug, Deserialize)]
pub struct CatalogResponse {
    pub errors: Vec<CatalogEntry>,
}

/// Parse an API error body into (optional error code, message string).
pub(crate) fn parse_api_error(body: &str) -> (Option<String>, String) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        let code = v
            .get("code")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        let message = v
            .get("error")
            .or_else(|| v.get("detail"))
            .or_else(|| v.get("message"))
            .and_then(|f| f.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| body.to_string());
        (code, message)
    } else {
        (None, body.to_string())
    }
}

/// Map an HTTP status + optional catalog code to a typed [`SynapseError`].
///
/// `code` is the raw `code` field parsed from the response body (see
/// [`parse_api_error`]), if present; it is threaded into [`ErrorCode`] on the
/// [`SynapseError::Api`] variant for statuses that don't already get one of
/// the dedicated named variants below.
pub fn map_status_to_error(status: u16, message: String, code: Option<String>) -> SynapseError {
    match status {
        401 => SynapseError::Unauthorized(message),
        403 => SynapseError::Forbidden(message),
        404 => SynapseError::NotFound(message),
        429 => SynapseError::RateLimited,
        _ => SynapseError::Api {
            status,
            message,
            code: code.map(|c| ErrorCode::from_code(&c)),
        },
    }
}

/// Parses `body` for a catalog `code` and maps it, with `status`, to a typed
/// [`SynapseError`]. Convenience wrapper around [`parse_api_error`] +
/// [`map_status_to_error`] for call sites that don't otherwise need the
/// intermediate catalog-description lookup that [`map_status_to_error`]'s
/// only other caller performs.
pub fn build_api_error(status: u16, body: String) -> SynapseError {
    let (code, message) = parse_api_error(&body);
    map_status_to_error(status, message, code)
}
