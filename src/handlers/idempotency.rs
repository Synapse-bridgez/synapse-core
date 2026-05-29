//! Idempotency key management handlers.
//!
//! Exposes an endpoint that allows API clients to validate an idempotency key
//! before sending a mutating request.  Clients that pre-validate keys receive
//! an early rejection (HTTP 400) for malformed keys instead of discovering the
//! problem mid-request after side-effects may have occurred.
//!
//! # Endpoint
//!
//! `GET /idempotency-key/validate`
//!
//! Reads the `x-idempotency-key` request header, runs it through the shared
//! [`validate_idempotency_key`] validator, and returns a JSON body describing
//! whether the key is acceptable and its normalised form.
//!
//! ## Request headers
//!
//! | Header | Required | Description |
//! |---|---|---|
//! | `x-idempotency-key` | Yes | The key to validate. |
//!
//! ## Responses
//!
//! | Status | Condition |
//! |---|---|
//! | 200 | Key is valid; body contains `{ "valid": true, "key": "<normalised>" }`. |
//! | 400 | Header is missing or the key is invalid; body contains `{ "valid": false, "error": "…" }`. |

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::middleware::idempotency::validate_idempotency_key;
use crate::ApiState;

/// Response body returned by [`validate_idempotency_key_handler`].
#[derive(Debug, Serialize, ToSchema)]
pub struct IdempotencyKeyValidationResponse {
    /// Whether the supplied key passes all validation rules.
    pub valid: bool,
    /// Normalised key value (trimmed whitespace). Present only when `valid` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Human-readable error message. Present only when `valid` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Validates the `x-idempotency-key` header value.
///
/// Reads the `x-idempotency-key` request header and runs it through the
/// shared key validator.  Returns a structured JSON response so clients
/// can detect malformed keys before issuing mutating requests.
///
/// # Validation rules (delegated to [`validate_idempotency_key`])
///
/// - Must not be empty or whitespace-only.
/// - Must not exceed 255 characters (after trimming).
/// - Must contain only `[A-Za-z0-9\-_.]` characters.
#[utoipa::path(
    get,
    path = "/idempotency-key/validate",
    responses(
        (status = 200, description = "Key is valid", body = IdempotencyKeyValidationResponse),
        (status = 400, description = "Header missing or key invalid", body = IdempotencyKeyValidationResponse),
    ),
    tag = "Idempotency"
)]
pub async fn validate_idempotency_key_handler(
    State(_state): State<ApiState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let raw_key = match headers.get("x-idempotency-key") {
        Some(value) => match value.to_str() {
            Ok(s) => s.to_owned(),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(IdempotencyKeyValidationResponse {
                        valid: false,
                        key: None,
                        error: Some("x-idempotency-key header contains non-UTF-8 bytes".into()),
                    }),
                )
                    .into_response();
            }
        },
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(IdempotencyKeyValidationResponse {
                    valid: false,
                    key: None,
                    error: Some("x-idempotency-key header is required".into()),
                }),
            )
                .into_response();
        }
    };

    match validate_idempotency_key(&raw_key) {
        Ok(normalised) => (
            StatusCode::OK,
            Json(IdempotencyKeyValidationResponse {
                valid: true,
                key: Some(normalised),
                error: None,
            }),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(IdempotencyKeyValidationResponse {
                valid: false,
                key: None,
                error: Some(e.to_string()),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid(key: &str) {
        let result = validate_idempotency_key(key);
        assert!(result.is_ok(), "expected valid for key {:?}", key);
    }

    fn assert_invalid(key: &str) {
        let result = validate_idempotency_key(key);
        assert!(result.is_err(), "expected invalid for key {:?}", key);
    }

    #[test]
    fn valid_keys_are_accepted() {
        assert_valid("order-abc123");
        assert_valid("payment_2024.01.15");
        assert_valid("req-uuid-1234-5678");
    }

    #[test]
    fn empty_and_whitespace_keys_are_rejected() {
        assert_invalid("");
        assert_invalid("   ");
    }

    #[test]
    fn keys_with_special_characters_are_rejected() {
        assert_invalid("key with spaces");
        assert_invalid("key@domain");
        assert_invalid("key/path");
        assert_invalid("key\x00null");
    }

    #[test]
    fn whitespace_trimming_normalises_key() {
        let result = validate_idempotency_key("  abc123  ").unwrap();
        assert_eq!(result, "abc123");
    }

    #[test]
    fn keys_exceeding_max_length_are_rejected() {
        let long_key = "a".repeat(256);
        assert_invalid(&long_key);
    }
}
