//! Axum handler that routes HTTP requests to [`TelemetryWebhookHandler`].
//!
//! This module wires the security-hardened telemetry webhook pipeline — HMAC
//! signature verification, payload-size cap, timestamp replay protection, and
//! field validation — to the `/telemetry/webhook` route so that requests
//! actually flow through the protections described in
//! `src/telemetry/webhook.rs` (fix for #976).

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::telemetry::webhook::TelemetryWebhookHandler;
use crate::ApiState;

/// Response body returned on a successful telemetry webhook ingestion.
#[derive(Debug, Serialize)]
pub struct TelemetryWebhookResponse {
    pub accepted: bool,
    pub message: String,
}

/// Receive and process an incoming telemetry webhook payload.
///
/// The handler delegates all security checks to [`TelemetryWebhookHandler`]:
/// - Payload size cap (64 KiB)
/// - HMAC-SHA256 signature verification (`X-Webhook-Signature` header)
/// - Timestamp replay-window check
/// - Structural field validation
///
/// # Responses
///
/// - `200 OK` — payload accepted and recorded.
/// - `400 Bad Request` — malformed JSON or failed field validation.
/// - `401 Unauthorized` — missing or invalid `X-Webhook-Signature`, or
///   timestamp outside the replay-protection window.
/// - `413 Payload Too Large` — payload exceeds 64 KiB.
/// - `500 Internal Server Error` — secret is not configured.
pub async fn handle_telemetry_webhook(
    State(state): State<ApiState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Resolve the webhook secret from the SecretsStore when available, or fall
    // back to the TELEMETRY_WEBHOOK_SECRET environment variable.
    let secret: Vec<u8> = if let Some(store) = &state.app_state.secrets_store {
        let secrets = store.valid_webhook_secrets().await;
        // Use the most recent (first) valid secret for signature verification.
        match secrets.into_iter().next() {
            Some(s) => s.into_bytes(),
            None => {
                tracing::error!("telemetry webhook: SecretsStore has no valid webhook secrets");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Telemetry webhook secret is not configured",
                )
                    .into_response();
            }
        }
    } else {
        match std::env::var("TELEMETRY_WEBHOOK_SECRET") {
            Ok(s) if !s.is_empty() => s.into_bytes(),
            _ => {
                tracing::error!("telemetry webhook: TELEMETRY_WEBHOOK_SECRET env var not set");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Telemetry webhook secret is not configured",
                )
                    .into_response();
            }
        }
    };

    // Build a handler instance (cheap — just wraps the key bytes after
    // validating they are non-empty).
    let handler = match TelemetryWebhookHandler::new(secret) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("telemetry webhook: failed to initialise handler: {e:?}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Telemetry webhook secret is not configured",
            )
                .into_response();
        }
    };

    // Extract the HMAC signature header.
    let signature = match headers
        .get(crate::telemetry::webhook::SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        Some(sig) => sig.to_owned(),
        None => {
            tracing::warn!(
                "telemetry webhook: missing {} header",
                crate::telemetry::webhook::SIGNATURE_HEADER
            );
            return (
                StatusCode::UNAUTHORIZED,
                "Missing X-Webhook-Signature header",
            )
                .into_response();
        }
    };

    // Delegate to the fully-tested security pipeline.
    match handler.process(&body, &signature) {
        Ok(result) => {
            let status = if result.accepted {
                StatusCode::OK
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            (
                status,
                axum::Json(TelemetryWebhookResponse {
                    accepted: result.accepted,
                    message: "Telemetry webhook processed".to_string(),
                }),
            )
                .into_response()
        }
        Err(e) => {
            use crate::telemetry::error_handling::TelemetryError;
            let (status, msg) = match &e {
                TelemetryError::PayloadTooLarge(_) => (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Payload exceeds the maximum allowed size",
                ),
                TelemetryError::ValidationError(_) => (
                    StatusCode::UNAUTHORIZED,
                    "Webhook signature or payload validation failed",
                ),
                _ => (StatusCode::BAD_REQUEST, "Invalid telemetry webhook payload"),
            };
            tracing::warn!("telemetry webhook rejected: {e:?}");
            (status, msg).into_response()
        }
    }
}
