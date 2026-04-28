//! Outgoing webhook dispatcher.
//!
//! Delivers signed HMAC-SHA256 payloads to registered endpoints when
//! transactions reach terminal states. Retries with exponential backoff
//! up to MAX_ATTEMPTS times and records every attempt in webhook_deliveries.

use chrono::Utc;
use futures::stream::{self, StreamExt};
use hmac::{Hmac, Mac};
use redis::{AsyncCommands, Client};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use sqlx::{PgPool, Row};
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
    pub max_delivery_rate: i32,
    pub filter_rules: Option<serde_json::Value>,
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
    pub max_delivery_rate: i32,
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
    http: HttpClient,
    redis: Client,
    concurrency: usize,
}

impl WebhookDispatcher {
    pub fn new(pool: PgPool, redis_url: &str) -> Result<Self, redis::RedisError> {
        let concurrency = std::env::var("WEBHOOK_DELIVERY_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10usize);
        Ok(Self {
            pool,
            http: HttpClient::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build reqwest client"),
            redis: Client::open(redis_url)?,
            concurrency,
        })
    }

    /// Enqueue deliveries for all enabled endpoints subscribed to `event_type`.
    /// Call this from TransactionProcessor on every terminal state transition.
    pub async fn enqueue(
        &self,
        transaction_id: Uuid,
        event_type: &str,
        data: serde_json::Value,
    ) -> anyhow::Result<()> {
        let endpoints = self.endpoints_for_event(event_type, &data).await?;
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
            let result = sqlx::query(
                r#"
                INSERT INTO webhook_deliveries
                    (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at)
                VALUES ($1, $2, $3, $4, 'pending', NOW())
                ON CONFLICT (endpoint_id, transaction_id, event_type) DO NOTHING
                "#,
            )
            .bind(ep.id)
            .bind(transaction_id)
            .bind(event_type)
            .bind(&payload)
            .execute(&self.pool)
            .await?;

            if result.rows_affected() == 0 {
                tracing::debug!(
                    endpoint_id = %ep.id,
                    transaction_id = %transaction_id,
                    event_type = event_type,
                    "Skipped duplicate webhook delivery"
                );
            }
        }

        Ok(())
    }

    /// Process all pending deliveries concurrently using `buffer_unordered`.
    pub async fn process_pending(&self) -> anyhow::Result<()> {
        let deliveries: Vec<WebhookDelivery> = sqlx::query_as(
            r#"
            SELECT wd.*, we.max_delivery_rate
            FROM webhook_deliveries wd
            JOIN webhook_endpoints we ON wd.endpoint_id = we.id
            WHERE wd.status = 'pending'
              AND (wd.next_attempt_at IS NULL OR wd.next_attempt_at <= NOW())
            ORDER BY wd.created_at
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

    async fn check_rate_limit(&self, endpoint_id: Uuid, max_rate: i32) -> anyhow::Result<bool> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = format!("webhook_rate:{endpoint_id}");

        // Use Redis INCR to atomically increment the counter
        // If the key doesn't exist, INCR sets it to 1
        let current_count: i32 = conn.incr(&key, 1).await?;

        // If this is the first request in the window, set expiry
        if current_count == 1 {
            let _: () = conn.expire(&key, 60).await?;
        }

        // Check if we're within the rate limit
        let allowed = current_count <= max_rate;
        if !allowed {
            tracing::warn!(
                endpoint_id = %endpoint_id,
                current_count = current_count,
                max_rate = max_rate,
                "Rate limit exceeded for webhook endpoint"
            );
        }

        Ok(allowed)
    }

    async fn attempt_delivery(&self, delivery: &WebhookDelivery) -> anyhow::Result<()> {
        // Check rate limit first
        if !self
            .check_rate_limit(delivery.endpoint_id, delivery.max_delivery_rate)
            .await?
        {
            // Rate limit exceeded, delay this delivery to next cycle
            let next_cycle = Utc::now() + chrono::Duration::seconds(30); // Next processing cycle
            sqlx::query(
                r#"
                UPDATE webhook_deliveries
                SET next_attempt_at = $1
                WHERE id = $2
                "#,
            )
            .bind(next_cycle)
            .bind(delivery.id)
            .execute(&self.pool)
            .await?;
            tracing::debug!(
                delivery_id = %delivery.id,
                endpoint_id = %delivery.endpoint_id,
                "Rate limit exceeded, delaying delivery to next cycle"
            );
            return Ok(());
        }

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

    async fn endpoints_for_event(
        &self,
        event_type: &str,
        transaction_data: &serde_json::Value,
    ) -> anyhow::Result<Vec<WebhookEndpoint>> {
        let all_endpoints: Vec<WebhookEndpoint> = sqlx::query_as(
            r#"
            SELECT * FROM webhook_endpoints
            WHERE enabled = TRUE
              AND $1 = ANY(event_types)
            "#,
        )
        .bind(event_type)
        .fetch_all(&self.pool)
        .await?;

        // Apply filter rules
        let mut filtered_endpoints = Vec::new();
        for endpoint in all_endpoints {
            if self.matches_filters(&endpoint, transaction_data) {
                filtered_endpoints.push(endpoint);
            }
        }

        Ok(filtered_endpoints)
    }

    pub fn matches_filters(
        &self,
        endpoint: &WebhookEndpoint,
        transaction_data: &serde_json::Value,
    ) -> bool {
        // If no filter rules, accept all
        let Some(filter_rules) = &endpoint.filter_rules else {
            return true;
        };

        // Extract transaction properties
        let asset_code = transaction_data.get("asset_code").and_then(|v| v.as_str());
        let amount_str = transaction_data.get("amount").and_then(|v| v.as_str());
        let amount = amount_str.and_then(|s| s.parse::<f64>().ok());

        // Check asset_codes filter
        if let Some(asset_codes) = filter_rules.get("asset_codes") {
            if let Some(asset_codes_array) = asset_codes.as_array() {
                if let Some(asset_code) = asset_code {
                    let allowed = asset_codes_array
                        .iter()
                        .filter_map(|v| v.as_str())
                        .any(|allowed_code| allowed_code == asset_code);
                    if !allowed {
                        return false;
                    }
                } else {
                    // If transaction has no asset_code but filter requires specific codes, reject
                    return false;
                }
            }
        }

        // Check min_amount filter
        if let Some(min_amount_str) = filter_rules.get("min_amount").and_then(|v| v.as_str()) {
            if let Ok(min_amount) = min_amount_str.parse::<f64>() {
                if let Some(amount) = amount {
                    if amount < min_amount {
                        return false;
                    }
                } else {
                    // If transaction has no amount but filter requires min_amount, reject
                    return false;
                }
            }
        }

        // Check max_amount filter
        if let Some(max_amount_str) = filter_rules.get("max_amount").and_then(|v| v.as_str()) {
            if let Ok(max_amount) = max_amount_str.parse::<f64>() {
                if let Some(amount) = amount {
                    if amount > max_amount {
                        return false;
                    }
                } else {
                    // If transaction has no amount but filter requires max_amount, reject
                    return false;
                }
            }
        }

        // Add more filters as needed (e.g., tenant, status, etc.)

        true
    }

    // -----------------------------------------------------------------------
    // Reliability tracking (per-endpoint success rate + auto-disable)
    // -----------------------------------------------------------------------

    /// Record a delivery event and recompute endpoint reliability stats.
    /// Called after every delivery attempt (success or failure).
    pub async fn record_delivery_event(
        &self,
        endpoint_id: Uuid,
        success: bool,
        http_status: Option<i32>,
        response_time_ms: i32,
        error_message: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO webhook_delivery_events
                (endpoint_id, delivered_at, success, http_status, response_time_ms, error_message)
            VALUES ($1, NOW(), $2, $3, $4, $5)
            "#,
        )
        .bind(endpoint_id)
        .bind(success)
        .bind(http_status)
        .bind(response_time_ms)
        .bind(error_message)
        .execute(&self.pool)
        .await?;

        self.update_endpoint_stats(endpoint_id).await?;
        Ok(())
    }

    /// Recompute success_rate and total_deliveries from the last 100 deliveries,
    /// then auto-disable the endpoint if the rate drops below 10%.
    async fn update_endpoint_stats(&self, endpoint_id: Uuid) -> anyhow::Result<()> {
        const ROLLING_WINDOW: i64 = 100;
        const AUTO_DISABLE_THRESHOLD: f64 = 10.0;

        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*)                                    AS total,
                SUM(CASE WHEN success THEN 1 ELSE 0 END)   AS successes
            FROM (
                SELECT success
                FROM webhook_delivery_events
                WHERE endpoint_id = $1
                ORDER BY delivered_at DESC
                LIMIT $2
            ) recent
            "#,
        )
        .bind(endpoint_id)
        .bind(ROLLING_WINDOW)
        .fetch_one(&self.pool)
        .await?;

        let total = row
            .try_get::<Option<i64>, _>("total")
            .unwrap_or(None)
            .unwrap_or(0) as i32;
        let successes = row
            .try_get::<Option<i64>, _>("successes")
            .unwrap_or(None)
            .unwrap_or(0) as f64;
        let success_rate = if total > 0 {
            (successes / total as f64) * 100.0
        } else {
            100.0
        };

        sqlx::query(
            r#"
            UPDATE webhook_endpoints
            SET
                success_rate     = $2,
                total_deliveries = $3,
                last_success_at  = CASE
                    WHEN (
                        SELECT success FROM webhook_delivery_events
                        WHERE endpoint_id = $1
                        ORDER BY delivered_at DESC
                        LIMIT 1
                    ) THEN NOW()
                    ELSE last_success_at
                END,
                updated_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(endpoint_id)
        .bind(success_rate)
        .bind(total)
        .execute(&self.pool)
        .await?;

        if success_rate < AUTO_DISABLE_THRESHOLD && total >= 100 {
            let updated = sqlx::query(
                r#"
                UPDATE webhook_endpoints
                SET enabled = FALSE, updated_at = NOW()
                WHERE id = $1 AND enabled = TRUE
                RETURNING id
                "#,
            )
            .bind(endpoint_id)
            .fetch_optional(&self.pool)
            .await?;

            if updated.is_some() {
                tracing::warn!(
                    endpoint_id = %endpoint_id,
                    success_rate = success_rate,
                    "Webhook endpoint auto-disabled due to low success rate"
                );

                sqlx::query(
                    r#"
                    INSERT INTO webhook_endpoint_notifications
                        (endpoint_id, reason, success_rate, notified_at)
                    VALUES ($1, 'auto_disabled_low_success_rate', $2, NOW())
                    "#,
                )
                .bind(endpoint_id)
                .bind(success_rate)
                .execute(&self.pool)
                .await?;
            }
        }

        Ok(())
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
    let signed_content = format!("{timestamp}.{body}");
    let signature_hex = sign_payload_v1(secret, &signed_content);
    format!("{SIGNATURE_VERSION}={signature_hex}")
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
            67,
            "v1 signature should be 67 chars (3 for 'v1=' + 64 for sha256 hex)"
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

    #[tokio::test]
    async fn test_filter_no_rules_accepts_all() {
        let dispatcher = WebhookDispatcher::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            "redis://dummy",
        )
        .unwrap();
        let endpoint = WebhookEndpoint {
            id: Uuid::new_v4(),
            url: "http://example.com".to_string(),
            secret: "secret".to_string(),
            event_types: vec!["transaction.completed".to_string()],
            enabled: true,
            max_delivery_rate: 10,
            filter_rules: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let transaction_data = serde_json::json!({
            "asset_code": "USD",
            "amount": "100.00"
        });

        assert!(dispatcher.matches_filters(&endpoint, &transaction_data));
    }

    #[tokio::test]
    async fn test_filter_asset_codes_matches() {
        let dispatcher = WebhookDispatcher::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            "redis://dummy",
        )
        .unwrap();
        let endpoint = WebhookEndpoint {
            id: Uuid::new_v4(),
            url: "http://example.com".to_string(),
            secret: "secret".to_string(),
            event_types: vec!["transaction.completed".to_string()],
            enabled: true,
            max_delivery_rate: 10,
            filter_rules: Some(serde_json::json!({"asset_codes": ["USD", "EUR"]})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let usd_transaction = serde_json::json!({
            "asset_code": "USD",
            "amount": "100.00"
        });
        let eur_transaction = serde_json::json!({
            "asset_code": "EUR",
            "amount": "200.00"
        });
        let btc_transaction = serde_json::json!({
            "asset_code": "BTC",
            "amount": "0.5"
        });

        assert!(dispatcher.matches_filters(&endpoint, &usd_transaction));
        assert!(dispatcher.matches_filters(&endpoint, &eur_transaction));
        assert!(!dispatcher.matches_filters(&endpoint, &btc_transaction));
    }

    #[tokio::test]
    async fn test_filter_min_amount() {
        let dispatcher = WebhookDispatcher::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            "redis://dummy",
        )
        .unwrap();
        let endpoint = WebhookEndpoint {
            id: Uuid::new_v4(),
            url: "http://example.com".to_string(),
            secret: "secret".to_string(),
            event_types: vec!["transaction.completed".to_string()],
            enabled: true,
            max_delivery_rate: 10,
            filter_rules: Some(serde_json::json!({"min_amount": "100.00"})),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let large_transaction = serde_json::json!({
            "asset_code": "USD",
            "amount": "150.00"
        });
        let small_transaction = serde_json::json!({
            "asset_code": "USD",
            "amount": "50.00"
        });

        assert!(dispatcher.matches_filters(&endpoint, &large_transaction));
        assert!(!dispatcher.matches_filters(&endpoint, &small_transaction));
    }

    #[tokio::test]
    async fn test_filter_combined_rules() {
        let dispatcher = WebhookDispatcher::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            "redis://dummy",
        )
        .unwrap();
        let endpoint = WebhookEndpoint {
            id: Uuid::new_v4(),
            url: "http://example.com".to_string(),
            secret: "secret".to_string(),
            event_types: vec!["transaction.completed".to_string()],
            enabled: true,
            max_delivery_rate: 10,
            filter_rules: Some(serde_json::json!({
                "asset_codes": ["USD"],
                "min_amount": "100.00",
                "max_amount": "1000.00"
            })),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let matching_transaction = serde_json::json!({
            "asset_code": "USD",
            "amount": "500.00"
        });
        let wrong_asset = serde_json::json!({
            "asset_code": "EUR",
            "amount": "500.00"
        });
        let too_small = serde_json::json!({
            "asset_code": "USD",
            "amount": "50.00"
        });
        let too_large = serde_json::json!({
            "asset_code": "USD",
            "amount": "1500.00"
        });

        assert!(dispatcher.matches_filters(&endpoint, &matching_transaction));
        assert!(!dispatcher.matches_filters(&endpoint, &wrong_asset));
        assert!(!dispatcher.matches_filters(&endpoint, &too_small));
        assert!(!dispatcher.matches_filters(&endpoint, &too_large));
    }

    // Note: Integration test for enqueue deduplication should verify that
    // calling enqueue twice for the same (endpoint_id, transaction_id, event_type)
    // creates only one delivery record due to the unique constraint and
    // ON CONFLICT DO NOTHING clause.
}

// ---------------------------------------------------------------------------
// Admin query helpers (used by handlers/admin.rs)
// ---------------------------------------------------------------------------

/// Snapshot of an endpoint's health as returned by the admin API.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct EndpointHealth {
    pub id: Uuid,
    pub url: String,
    pub enabled: bool,
    pub success_rate: f64,
    pub total_deliveries: i32,
    pub last_success_at: Option<chrono::DateTime<Utc>>,
}

/// Return health scores for all webhook endpoints.
pub async fn list_endpoint_health(
    pool: &PgPool,
) -> Result<Vec<EndpointHealth>, crate::error::AppError> {
    let rows = sqlx::query(
        r#"
        SELECT id, url, enabled, success_rate, total_deliveries, last_success_at
        FROM webhook_endpoints
        ORDER BY success_rate ASC, total_deliveries DESC
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(crate::error::AppError::Database)?;

    Ok(rows
        .into_iter()
        .map(|r: sqlx::postgres::PgRow| EndpointHealth {
            id: r.get("id"),
            url: r.get("url"),
            enabled: r.get("enabled"),
            success_rate: r
                .try_get::<sqlx::types::BigDecimal, _>("success_rate")
                .ok()
                .map(|v| v.to_string().parse::<f64>().unwrap_or(0.0))
                .unwrap_or(100.0),
            total_deliveries: r
                .try_get::<Option<i32>, _>("total_deliveries")
                .unwrap_or(None)
                .unwrap_or(0),
            last_success_at: r.try_get("last_success_at").unwrap_or(None),
        })
        .collect())
}

/// Return health score for a single endpoint.
pub async fn get_endpoint_health(
    pool: &PgPool,
    endpoint_id: Uuid,
) -> Result<EndpointHealth, crate::error::AppError> {
    let r = sqlx::query(
        r#"
        SELECT id, url, enabled, success_rate, total_deliveries, last_success_at
        FROM webhook_endpoints
        WHERE id = $1
        "#,
    )
    .bind(endpoint_id)
    .fetch_optional(pool)
    .await
    .map_err(crate::error::AppError::Database)?
    .ok_or_else(|| crate::error::AppError::NotFound(format!("Endpoint {endpoint_id} not found")))?;

    Ok(EndpointHealth {
        id: r.get("id"),
        url: r.get("url"),
        enabled: r.get("enabled"),
        success_rate: r
            .try_get::<sqlx::types::BigDecimal, _>("success_rate")
            .ok()
            .map(|v| v.to_string().parse::<f64>().unwrap_or(0.0))
            .unwrap_or(100.0),
        total_deliveries: r
            .try_get::<Option<i32>, _>("total_deliveries")
            .unwrap_or(None)
            .unwrap_or(0),
        last_success_at: r.try_get("last_success_at").unwrap_or(None),
    })
}
