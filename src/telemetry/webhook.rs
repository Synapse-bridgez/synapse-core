//! Secure webhook handler for the Telemetry module.
//!
//! Validates and processes incoming webhook payloads that carry OpenTelemetry
//! tracing events from external services.  Every payload is authenticated,
//! size-checked, and structurally validated before any tracing data is
//! recorded, preventing injection, replay, and resource-exhaustion attacks.
//!
//! # Security guarantees
//!
//! - **HMAC-SHA256 signature verification** — payloads without a valid
//!   `X-Webhook-Signature` header are rejected before deserialization.
//! - **Payload size cap** — payloads exceeding [`MAX_PAYLOAD_BYTES`] are
//!   rejected immediately to prevent memory exhaustion.
//! - **Structural validation** — event type, source, and span fields are
//!   validated via [`crate::telemetry::input_validation::InputValidator`]
//!   before any tracing call is made.
//! - **Replay protection** — each payload carries a `timestamp_ms` field;
//!   payloads older than [`MAX_TIMESTAMP_SKEW_SECS`] are rejected.
//! - **Constant-time comparison** — HMAC verification uses `subtle::ConstantTimeEq`
//!   to prevent timing side-channels.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::telemetry::error_handling::TelemetryError;
use crate::telemetry::input_validation::InputValidator;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum accepted payload size in bytes (64 KiB).
///
/// Payloads larger than this are rejected before deserialization to prevent
/// memory exhaustion attacks.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Maximum allowed clock skew between the sender and receiver in seconds.
///
/// Payloads with a `timestamp_ms` older than this window are rejected as
/// potential replays.
pub const MAX_TIMESTAMP_SKEW_SECS: u64 = 300; // 5 minutes

/// Header name carrying the HMAC-SHA256 signature.
pub const SIGNATURE_HEADER: &str = "X-Webhook-Signature";

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------

/// An incoming webhook payload carrying a telemetry tracing event.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct WebhookEventPayload {
    /// Milliseconds since Unix epoch when the event was emitted.
    pub timestamp_ms: u64,

    /// Source service that emitted the event (validated as an identifier).
    pub source: String,

    /// Event type (validated as an identifier).
    pub event_type: String,

    /// Optional span name associated with the event.
    pub span_name: Option<String>,

    /// Optional key-value attributes attached to the event.
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, String>,
}

/// Result of processing a webhook payload.
#[derive(Debug, Clone)]
pub struct WebhookResult {
    /// Whether the payload was accepted and recorded.
    pub accepted: bool,
    /// Human-readable status message.
    pub message: String,
}

impl WebhookResult {
    fn accepted(message: impl Into<String>) -> Self {
        Self {
            accepted: true,
            message: message.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Secure webhook handler for telemetry tracing events.
///
/// Validates signatures, enforces size limits, checks timestamps for replay
/// protection, and validates all payload fields before recording spans.
#[derive(Debug, Clone)]
pub struct TelemetryWebhookHandler {
    /// HMAC secret used to verify incoming webhook signatures.
    secret: Vec<u8>,
}

impl TelemetryWebhookHandler {
    /// Creates a new handler with the given HMAC secret.
    ///
    /// # Errors
    /// Returns [`TelemetryError::PoolConfigError`] if the secret is empty.
    pub fn new(secret: impl Into<Vec<u8>>) -> Result<Self, TelemetryError> {
        let secret = secret.into();
        if secret.is_empty() {
            return Err(TelemetryError::PoolConfigError(
                "webhook HMAC secret must not be empty".into(),
            ));
        }
        Ok(Self { secret })
    }

    /// Processes a raw webhook request.
    ///
    /// Steps performed in order:
    /// 1. Reject oversized payloads.
    /// 2. Verify HMAC-SHA256 signature.
    /// 3. Deserialize JSON body.
    /// 4. Validate timestamp (replay protection).
    /// 5. Validate all string fields.
    /// 6. Record the tracing span.
    ///
    /// # Arguments
    /// - `body` — raw request body bytes.
    /// - `signature` — value of the `X-Webhook-Signature` header (hex-encoded HMAC).
    ///
    /// # Errors
    /// Returns a [`TelemetryError`] variant describing the first validation failure.
    pub fn process(&self, body: &[u8], signature: &str) -> Result<WebhookResult, TelemetryError> {
        // 1. Size check — before any allocation-heavy work.
        if body.len() > MAX_PAYLOAD_BYTES {
            tracing::warn!(
                size = body.len(),
                max = MAX_PAYLOAD_BYTES,
                "Telemetry webhook rejected: payload too large"
            );
            return Err(TelemetryError::PayloadTooLarge(MAX_PAYLOAD_BYTES));
        }

        // 2. Signature verification.
        self.verify_signature(body, signature)?;

        // 3. Deserialize.
        let payload: WebhookPayload = serde_json::from_slice(body).map_err(|e| {
            TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::InvalidFormat(format!(
                    "invalid JSON: {}",
                    e
                )),
            )
        })?;

        // 4. Replay protection.
        self.check_timestamp(payload.timestamp_ms)?;

        // 5. Field validation.
        self.validate_payload(&payload)?;

        // 6. Record span.
        tracing::info!(
            source = %payload.source,
            event_type = %payload.event_type,
            span_name = ?payload.span_name,
            "Telemetry webhook event received"
        );

        Ok(WebhookResult::accepted("event recorded"))
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Verifies the HMAC-SHA256 signature of the raw body.
    ///
    /// Uses constant-time comparison to prevent timing side-channels.
    fn verify_signature(&self, body: &[u8], signature: &str) -> Result<(), TelemetryError> {
        if signature.is_empty() {
            return Err(TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::EmptyValue(
                    "missing webhook signature".into(),
                ),
            ));
        }

        let expected = hex::decode(signature).map_err(|_| {
            TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::InvalidFormat(
                    "signature must be hex-encoded".into(),
                ),
            )
        })?;

        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).map_err(|e| {
            TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::InvalidFormat(format!(
                    "HMAC init error: {}",
                    e
                )),
            )
        })?;
        mac.update(body);

        mac.verify_slice(&expected).map_err(|_| {
            tracing::warn!("Telemetry webhook rejected: invalid signature");
            TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::InvalidFormat(
                    "invalid webhook signature".into(),
                ),
            )
        })
    }

    /// Checks that the payload timestamp is within the allowed skew window.
    fn check_timestamp(&self, timestamp_ms: u64) -> Result<(), TelemetryError> {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let skew_ms = MAX_TIMESTAMP_SKEW_SECS * 1_000;

        // Reject if timestamp is too far in the past or future.
        let too_old = now_ms.saturating_sub(timestamp_ms) > skew_ms;
        let too_new = timestamp_ms.saturating_sub(now_ms) > skew_ms;

        if too_old || too_new {
            tracing::warn!(
                timestamp_ms,
                now_ms,
                "Telemetry webhook rejected: timestamp outside allowed window"
            );
            return Err(TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::InvalidFormat(
                    "webhook timestamp is outside the allowed window (possible replay)".into(),
                ),
            ));
        }

        Ok(())
    }

    /// Validates all string fields in the payload.
    fn validate_payload(&self, payload: &WebhookPayload) -> Result<(), TelemetryError> {
        // source and event_type must be valid identifiers.
        InputValidator::validate_span_name(&payload.source)?;
        InputValidator::validate_span_name(&payload.event_type)?;

        // span_name is optional but must be valid if present.
        if let Some(span_name) = &payload.span_name {
            InputValidator::validate_span_name(span_name)?;
        }

        // Validate all attribute values.
        for (key, value) in &payload.attributes {
            InputValidator::validate_span_name(key)?;
            InputValidator::validate_attribute_value(value)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    const SECRET: &[u8] = b"test-secret-key-for-unit-tests";

    fn sign(body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn valid_body(ts: u64) -> Vec<u8> {
        serde_json::json!({
            "timestamp_ms": ts,
            "source": "payment-service",
            "event_type": "settlement_completed",
            "span_name": "process_settlement",
            "attributes": {}
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn empty_secret_rejected() {
        assert!(TelemetryWebhookHandler::new(b"".to_vec()).is_err());
    }

    #[test]
    fn valid_payload_accepted() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = valid_body(now_ms());
        let sig = sign(&body);
        let result = handler.process(&body, &sig).unwrap();
        assert!(result.accepted);
    }

    #[test]
    fn invalid_signature_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = valid_body(now_ms());
        let result = handler.process(&body, "deadbeef");
        assert!(result.is_err());
    }

    #[test]
    fn missing_signature_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = valid_body(now_ms());
        let result = handler.process(&body, "");
        assert!(result.is_err());
    }

    #[test]
    fn oversized_payload_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = vec![0u8; MAX_PAYLOAD_BYTES + 1];
        let sig = sign(&body);
        let result = handler.process(&body, &sig);
        assert!(matches!(result, Err(TelemetryError::PayloadTooLarge(_))));
    }

    #[test]
    fn stale_timestamp_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        // Timestamp 10 minutes in the past.
        let old_ts = now_ms().saturating_sub(600_000);
        let body = valid_body(old_ts);
        let sig = sign(&body);
        let result = handler.process(&body, &sig);
        assert!(result.is_err());
    }

    #[test]
    fn future_timestamp_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        // Timestamp 10 minutes in the future.
        let future_ts = now_ms() + 600_000;
        let body = valid_body(future_ts);
        let sig = sign(&body);
        let result = handler.process(&body, &sig);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_json_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = b"not json at all";
        let sig = sign(body);
        let result = handler.process(body, &sig);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_source_field_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = serde_json::json!({
            "timestamp_ms": now_ms(),
            "source": "bad source with spaces!",
            "event_type": "test_event",
            "attributes": {}
        })
        .to_string()
        .into_bytes();
        let sig = sign(&body);
        let result = handler.process(&body, &sig);
        assert!(result.is_err());
    }

    #[test]
    fn non_hex_signature_rejected() {
        let handler = TelemetryWebhookHandler::new(SECRET.to_vec()).unwrap();
        let body = valid_body(now_ms());
        let result = handler.process(&body, "not-hex!!");
        assert!(result.is_err());
    }
}
