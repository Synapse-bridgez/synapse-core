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
use sha2::Sha256;
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
        let signature = sign_payload(&endpoint.secret, &body);

        let response = self
            .http
            .post(&endpoint.url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", format!("sha256={signature}"))
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

/// Compute HMAC-SHA256 hex signature for a payload.
fn sign_payload(secret: &str, body: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
