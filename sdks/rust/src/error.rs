use thiserror::Error;

/// Errors returned by the Synapse SDK.
#[derive(Debug, Error)]
pub enum SynapseError {
    /// The server returned a non-success HTTP status.
    ///
    /// 5xx responses are transient (retryable). 4xx responses are permanent
    /// caller mistakes and are never retried.
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },

    /// Alias for `Http` used by resource methods to surface semantic errors.
    ///
    /// `status` is the HTTP status code; `message` is the response body.
    /// Resource methods map specific status codes (e.g. 404 → `NotFound`,
    /// 400 cursor → `InvalidCursor`) before returning this variant.
    #[error("API error {status}: {message}")]
    Api { status: u16, message: String },

    /// The requested resource was not found (HTTP 404).
    #[error("not found: {0}")]
    NotFound(String),

    /// A cursor provided for pagination was malformed or has expired (HTTP 400).
    ///
    /// Callers **must not** retry with the same cursor. Restart pagination
    /// from the beginning.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// The response body could not be decoded as the expected type.
    #[error("decode error: {0}")]
    Decode(String),

    /// A network-level failure occurred before a response was received.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}

impl SynapseError {
    /// Returns `true` if this error may resolve on a subsequent attempt.
    ///
    /// Network errors and 5xx HTTP responses are transient. 4xx responses are
    /// permanent (they represent a caller mistake) and must not be retried.
    pub fn is_transient(&self) -> bool {
        match self {
            SynapseError::Network(_) => true,
            SynapseError::Http { status, .. } => *status >= 500,
            SynapseError::Api { status, .. } => *status >= 500,
            SynapseError::NotFound(_)
            | SynapseError::InvalidCursor(_)
            | SynapseError::Decode(_) => false,
        }
    }
}
