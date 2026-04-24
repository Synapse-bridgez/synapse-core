//! Outgoing webhook dispatcher.
//!
//! Delivers signed HMAC-SHA256 payloads to registered endpoints when
//! transactions reach terminal states. Retries with exponential backoff
//! up to MAX_ATTEMPTS times and records every attempt in webhook_deliveries.

use chrono::Utc;
use futures::stream::{self, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use sqlx::PgPool;
use uuid::Uuid;

const MAX_ATTEMPTS: i32 = 5;
/// Base delay in seconds for exponential backoff (2^attempt * BASE_DELAY_SECS)
const BASE_DELAY_SECS: i64 = 10;

// ── Domain types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub url: String,
    pub secret: String,
    pub event_types: Vec<String>,
    pub enabled: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub updated_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub transaction_id: Uuid,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub attempt_count: i32,
    pub last_attempt_at: Option<chrono::DateTime<Utc>>,
    pub next_attempt_at: Option<chrono::DateTime<Utc>>,
    pub status: String,
    pub response_status: Option<i32>,
    pub response_body: Option<String>,
    pub created_at: chrono::DateTime<Utc>,
}

/// Payload sent to external endpoints.
#[derive(Debug, Serialize)]
pub struct OutgoingPayload {
    pub event_type: String,
    pub transaction_id: String,
    pub timestamp: chrono::DateTime<Utc>,
    pub data: serde_json::Value,
}

// ── Service ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebhookDispatcher {
    pool: PgPool,
    http: Client,
    concurrency: usize,
}

impl WebhookDispatcher {
    pub fn new(pool: PgPool) -> Self {
        let concurrency = std::env::var("WEBHOOK_DELIVERY_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10usize);
        Self {
            pool,
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            concurrency,
        }
    }

    /// Enqueue deliveries for all enabled endpoints subscribed to `event_type`.
    /// Call this from TransactionProcessor on every terminal state transition.
    pub async fn enqueue(
        &self,
        transaction_id: Uuid,
        event_type: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<()> {
        let endpoints = self.endpoints_for_event(event_type).await?;
        if endpoints.is_empty() {
            return Ok(());
        }

        let payload = serde_json::to_value(OutgoingPayload {
            event_type: event_type.to_string(),
            transaction_id: transaction_id.to_string(),
            timestamp: Utc::now(),
            data,
        })?;

        for ep in endpoints {
            sqlx::query(
                r#"
                INSERT INTO webhook_deliveries
                    (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
                VALUES ($1, $2, $3, $4, 'pending', NOW())
                "#,
            )
            .bind(ep.id)
            .bind(transaction_id)
            .bind(event_type)
            .bind(&payload)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Process all pending deliveries concurrently using `buffer_unordered`.
    pub async fn process_pending(&self) -> anyhow::Result<()> {
        let deliveries: Vec<WebhookDelivery> = sqlx::query_as(
            r#"
            SELECT * FROM webhook_deliveries
            WHERE status = 'pending'
              AND (next_attempt_at IS NULL OR next_attempt_at <= NOW())
            ORDER BY created_at
            LIMIT 100
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        stream::iter(deliveries)
            .map(|delivery| {
                let dispatcher = self.clone();
                async move {
                    let start = std::time::Instant::now();
                    if let Err(e) = dispatcher.attempt_delivery(&delivery).await {
                        tracing::error!(
                            delivery_id = %delivery.id,
                            "Webhook delivery attempt error: {e}"
                        );
                    }
                    let latency_ms = start.elapsed().as_millis() as u64;
                    tracing::debug!(
                        delivery_id = %delivery.id,
                        webhook_delivery_latency_ms = latency_ms,
                        "Webhook delivery attempt completed"
                    );
                }
            })
            .buffer_unordered(self.concurrency)
            .collect::<()>()
            .await;

        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    async fn attempt_delivery(&self, delivery: &WebhookDelivery) -> anyhow::Result<()> {
        let endpoint: WebhookEndpoint =
            sqlx::query_as("SELECT * FROM webhook_endpoints WHERE id = $1")
                .bind(delivery.endpoint_id)
                .fetch_one(&self.pool)
                .await?;

        let body = serde_json::to_string(&delivery.payload)?;

        // Extract timestamp from payload (OutgoingPayload includes timestamp field)
        let timestamp = delivery
            .payload
            .get("timestamp")
            .and_then(|ts| ts.as_str())
            .map(|ts| ts.to_string())
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        let signature = sign_payload_with_version(&endpoint.secret, &timestamp, &body);

        let response = self
            .http
            .post(&endpoint.url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", &signature)
            .header("X-Webhook-Timestamp", &timestamp)
            .header("X-Webhook-Event", &delivery.event_type)
            .body(body)
            .send()
            .await;

        let new_attempt_count = delivery.attempt_count + 1;
        let now = Utc::now();

        match response {
            Ok(resp) => {
                let status_code = resp.status().as_u16() as i32;
                let resp_body = resp.text().await.unwrap_or_default();
                let success = (200..300).contains(&(status_code as u16));

                if success {
                    sqlx::query(
                        r#"
                        UPDATE webhook_deliveries
                        SET status = 'delivered',
                            attempt_count = $1,
                            last_attempt_at = $2,
                            response_status = $3,
                            response_body = $4
                        WHERE id = $5
                        "#,
                    )
                    .bind(new_attempt_count)
                    .bind(now)
                    .bind(status_code)
                    .bind(&resp_body)
                    .bind(delivery.id)
                    .execute(&self.pool)
                    .await?;

                    tracing::info!(
                        delivery_id = %delivery.id,
                        endpoint = %endpoint.url,
                        "Webhook delivered successfully"
                    );
                } else {
                    self.handle_failure(
                        delivery,
                        new_attempt_count,
                        now,
                        Some(status_code),
                        Some(resp_body),
                    )
                    .await?;
                }
            }
            Err(e) => {
                self.handle_failure(delivery, new_attempt_count, now, None, Some(e.to_string()))
                    .await?;
            }
        }

        Ok(())
    }

    async fn handle_failure(
        &self,
        delivery: &WebhookDelivery,
        attempt_count: i32,
        now: chrono::DateTime<Utc>,
        response_status: Option<i32>,
        response_body: Option<String>,
    ) -> anyhow::Result<()> {
        let (new_status, next_attempt_at) = if attempt_count >= MAX_ATTEMPTS {
            tracing::warn!(
                delivery_id = %delivery.id,
                "Webhook delivery permanently failed after {} attempts",
                attempt_count
            );
            ("failed", None)
        } else {
            let delay = BASE_DELAY_SECS * (1_i64 << attempt_count);
            let next = now + chrono::Duration::seconds(delay);
            tracing::warn!(
                delivery_id = %delivery.id,
                attempt = attempt_count,
                next_retry_in_secs = delay,
                "Webhook delivery failed, scheduling retry"
            );
            ("pending", Some(next))
        };

        sqlx::query(
            r#"
            UPDATE webhook_deliveries
            SET status = $1,
                attempt_count = $2,
                last_attempt_at = $3,
                next_attempt_at = $4,
                response_status = $5,
                response_body = $6
            WHERE id = $7
            "#,
        )
        .bind(new_status)
        .bind(attempt_count)
        .bind(now)
        .bind(next_attempt_at)
        .bind(response_status)
        .bind(response_body)
        .bind(delivery.id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn endpoints_for_event(&self, event_type: &str) -> anyhow::Result<Vec<WebhookEndpoint>> {
        let endpoints: Vec<WebhookEndpoint> = sqlx::query_as(
            r#"
            SELECT * FROM webhook_endpoints
            WHERE enabled = TRUE
              AND $1 = ANY(event_types)
            "#,
        )
        .bind(event_type)
        .fetch_all(&self.pool)
        .await?;

        Ok(endpoints)
    }
}

/// Signature versions supported by the webhook system.
const SIGNATURE_VERSION: &str = "v1";

/// Compute versioned HMAC signature for a payload with timestamp.
///
/// # Signature Format
/// Returns: `v1=sha256_hex_value`
///
/// # Signed Content
/// The signed content is formatted as: `timestamp.body`
/// where timestamp is included in the X-Webhook-Timestamp header.
fn sign_payload_with_version(secret: &str, timestamp: &str, body: &str) -> String {
    let signed_content = format!("{}.{}", timestamp, body);
    let signature_hex = sign_payload_v1(secret, &signed_content);
    format!("{}={}", SIGNATURE_VERSION, signature_hex)
}

/// Compute HMAC-SHA256 hex signature (v1).
fn sign_payload_v1(secret: &str, signed_content: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(signed_content.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Prepare structure for v2 (HMAC-SHA512).
/// Currently returns the same as v1 for compatibility.
#[allow(dead_code)]
fn sign_payload_v2(secret: &str, signed_content: &str) -> String {
    let mut mac =
        Hmac::<Sha512>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(signed_content.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Compute HMAC-SHA256 hex signature for a payload (legacy).
/// This is deprecated in favor of sign_payload_with_version.
#[allow(dead_code)]
fn sign_payload(secret: &str, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v1_signature_includes_timestamp() {
        let secret = "test-secret";
        let timestamp = "2025-01-15T10:30:00Z";
        let body = r#"{"transaction_id":"123","status":"completed"}"#;

        let signature = sign_payload_with_version(secret, timestamp, body);

        // Verify signature format: v1=<hex>
        assert!(
            signature.starts_with("v1="),
            "Signature should start with v1="
        );
        assert_eq!(
            signature.len(),
            68,
            "v1 signature should be 68 chars (4 for 'v1=' + 64 for sha256 hex)"
        );
    }

    #[test]
    fn test_v1_signature_matches_expected_value() {
        let secret = "webhook-secret";
        let timestamp = "2025-01-15T10:30:00Z";
        let body = r#"{"id":"txn-123"}"#;

        // Compute expected signature manually
        let signed_content = format!("{}.{}", timestamp, body);
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(signed_content.as_bytes());
        let expected_hex = hex::encode(mac.finalize().into_bytes());
        let expected_signature = format!("v1={}", expected_hex);

        let signature = sign_payload_with_version(secret, timestamp, body);

        assert_eq!(
            signature, expected_signature,
            "Signature should match expected value"
        );
    }

    #[test]
    fn test_different_timestamps_produce_different_signatures() {
        let secret = "webhook-secret";
        let body = r#"{"id":"txn-123"}"#;

        let sig1 = sign_payload_with_version(secret, "2025-01-15T10:30:00Z", body);
        let sig2 = sign_payload_with_version(secret, "2025-01-15T10:30:01Z", body);

        assert_ne!(
            sig1, sig2,
            "Different timestamps should produce different signatures"
        );
    }

    #[test]
    fn test_timestamp_in_signed_content() {
        let secret = "webhook-secret";
        let timestamp = "2025-01-15T10:30:00Z";
        let body = r#"{"id":"txn-123"}"#;

        // Verify by computing signature with timestamp included
        let sig_with_ts = sign_payload_with_version(secret, timestamp, body);

        // Verify that body alone would produce different signature
        let old_style_hex = sign_payload(secret, body);
        let old_style_sig = format!("v1={}", old_style_hex);

        assert_ne!(
            sig_with_ts, old_style_sig,
            "Signature with timestamp should differ from signature without timestamp"
        );
    }

    #[test]
    fn test_v1_signature_hex_encoding() {
        let secret = "test";
        let timestamp = "2025-01-15T10:30:00Z";
        let body = "{}";

        let signature = sign_payload_with_version(secret, timestamp, body);

        // Remove v1= prefix and verify it's valid hex
        let hex_part = &signature[3..];
        assert!(
            hex_part.chars().all(|c| c.is_ascii_hexdigit()),
            "Signature hex should contain only valid hex characters"
        );
        assert_eq!(hex_part.len(), 64, "SHA256 hex should be 64 characters");
    }

    #[test]
    fn test_v1_signature_deterministic() {
        let secret = "webhook-secret";
        let timestamp = "2025-01-15T10:30:00Z";
        let body = r#"{"id":"txn-123"}"#;

        let sig1 = sign_payload_with_version(secret, timestamp, body);
        let sig2 = sign_payload_with_version(secret, timestamp, body);

        assert_eq!(
            sig1, sig2,
            "Signature should be deterministic for same inputs"
        );
    }
}
