use serde::Deserialize;
use thiserror::Error;

/// Errors returned by the Synapse SDK.
#[derive(Debug, Error)]
pub enum SynapseError {
    /// A structured API error returned by the server (non-2xx response).
    ///
    /// 5xx responses are transient (retryable). 4xx responses are permanent
    /// caller mistakes and are never retried.
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },

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
    #[error("rate limit exceeded")]
    RateLimited,

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

    /// A non-success API response with a structured message.
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },

    /// The requested resource was not found (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),

    /// A pagination cursor was rejected by the server (HTTP 400 with cursor error).
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// The GraphQL response contained application-level errors (HTTP 200 with `errors` array).
    #[error("GraphQL errors: {0}")]
    GraphQL(String),

    /// A network-level failure occurred before a response was received.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// The requested resource was not found (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),

    /// The pagination cursor is malformed or expired (HTTP 400 with cursor message).
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// The server returned a non-success status with a structured error message.
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },
}

impl SynapseError {
    /// Returns `true` if this error may resolve on a subsequent attempt.
    ///
    /// Network errors and 5xx HTTP responses are transient. 4xx responses are
    /// permanent (they represent a caller mistake) and must not be retried.
    pub fn is_transient(&self) -> bool {
        match self {
            SynapseError::Network(_) => true,
            SynapseError::Http { status, .. } | SynapseError::Api { status, .. } => *status >= 500,
            _ => false,
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
        let code = v.get("code").and_then(|c| c.as_str()).map(|s| s.to_string());
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

/// Map an HTTP status + optional catalog lookup to a typed [`SynapseError`].
pub(crate) fn map_status_to_error(
    status: u16,
    message: String,
) -> SynapseError {
    match status {
        401 => SynapseError::Unauthorized(message),
        403 => SynapseError::Forbidden(message),
        404 => SynapseError::NotFound(message),
        429 => SynapseError::RateLimited,
        _ => SynapseError::Api { status, message },
    }
}
