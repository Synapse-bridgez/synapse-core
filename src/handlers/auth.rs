use axum::{
    async_trait,
    body::Body,
    extract::{FromRequest, Request},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::cache::webhook as webhook_security;

type HmacSha256 = Hmac<Sha256>;

/// Extractor that verifies the X-Stellar-Signature header
/// against the request body using HMAC-SHA256
pub struct VerifiedWebhook {
    pub body: Vec<u8>,
}

impl VerifiedWebhook {
    /// Verify the signature using constant-time comparison
    fn verify_signature(secret: &str, body: &[u8], signature_header: &str) -> Result<(), AuthError> {
        // Decode the hex signature from the header
        let expected_signature = hex::decode(signature_header)
            .map_err(|_| AuthError::InvalidSignatureFormat)?;

        // Compute HMAC-SHA256 of the body
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
            .map_err(|_| AuthError::InvalidSecret)?;
        mac.update(body);

        // Constant-time comparison to prevent timing attacks
        mac.verify_slice(&expected_signature)
            .map_err(|_| AuthError::SignatureMismatch)?;

        Ok(())
    }
}

#[async_trait]
impl FromRequest<crate::AppState> for VerifiedWebhook {
    type Rejection = AuthError;

    async fn from_request(req: Request, state: &crate::AppState) -> Result<Self, Self::Rejection> {
        let headers = req.headers().clone();

        // --- Replay protection (fix #977) -------------------------------------
        // Require an X-Webhook-Timestamp header and reject payloads whose
        // timestamp is more than 5 minutes old or from the future.  This
        // prevents a captured, validly-signed request from being replayed
        // indefinitely, since the signature alone is deterministic.
        let timestamp_str = headers
            .get("X-Webhook-Timestamp")
            .and_then(|v| v.to_str().ok())
            .ok_or(AuthError::MissingTimestamp)?;

        webhook_security::validate_timestamp(timestamp_str)
            .map_err(|_| AuthError::TimestampOutOfRange)?;
        // ----------------------------------------------------------------------

        // Extract the signature header
        let signature = headers
            .get("X-Stellar-Signature")
            .and_then(|v| v.to_str().ok())
            .ok_or(AuthError::MissingSignature)?;

        // Extract the body
        let body_bytes = axum::body::to_bytes(req.into_body(), usize::MAX)
            .await
            .map_err(|_| AuthError::BodyReadError)?
            .to_vec();

        // Verify the signature
        Self::verify_signature(&state.config.anchor_webhook_secret, &body_bytes, signature)?;

        Ok(VerifiedWebhook { body: body_bytes })
    }
}

#[derive(Debug)]
pub enum AuthError {
    MissingSignature,
    InvalidSignatureFormat,
    InvalidSecret,
    SignatureMismatch,
    BodyReadError,
    /// The X-Webhook-Timestamp header is absent.
    MissingTimestamp,
    /// The timestamp is outside the 5-minute replay-protection window.
    TimestampOutOfRange,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuthError::MissingSignature => {
                (StatusCode::UNAUTHORIZED, "Missing X-Stellar-Signature header")
            }
            AuthError::InvalidSignatureFormat => {
                (StatusCode::UNAUTHORIZED, "Invalid signature format")
            }
            AuthError::InvalidSecret => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Invalid webhook secret configuration")
            }
            AuthError::SignatureMismatch => {
                (StatusCode::UNAUTHORIZED, "Signature verification failed")
            }
            AuthError::BodyReadError => {
                (StatusCode::BAD_REQUEST, "Failed to read request body")
            }
            AuthError::MissingTimestamp => {
                (StatusCode::BAD_REQUEST, "Missing X-Webhook-Timestamp header")
            }
            AuthError::TimestampOutOfRange => {
                (StatusCode::UNAUTHORIZED, "Webhook timestamp outside the allowed window (replay protection)")
            }
        };

        tracing::warn!("Webhook authentication failed: {:?}", self);
        (status, message).into_response()
    }
}
