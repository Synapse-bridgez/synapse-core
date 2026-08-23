//! Outgoing webhook dispatcher.
//!
//! Delivers signed HMAC-SHA256 payloads to registered endpoints when
//! transactions reach terminal states. Retries with exponential backoff
//! up to MAX_ATTEMPTS times and records every attempt in webhook_deliveries.

use chrono::Utc;
use futures::stream::{self, StreamExt};
use hmac::{Hmac, Mac};
use redis::{AsyncCommands, Client, Script};
use reqwest::Client as HttpClient;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Sha512};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

const MAX_ATTEMPTS: i32 = 5;
/// Base delay in seconds for exponential backoff (2^attempt * BASE_DELAY_SECS)
const BASE_DELAY_SECS: i64 = 10;
/// How long (seconds) before a claimed in_progress delivery can be reclaimed
/// by another worker (crash recovery).
const CLAIM_TIMEOUT_SECS: i64 = 300;
/// Circuit breaker: consecutive failures before tripping open.
const CB_FAILURE_THRESHOLD: u32 = 3;
/// Circuit breaker: seconds before an open breaker transitions to half-open.
const CB_RESET_TIMEOUT_SECS: i64 = 300;
/// Atomically increments the rate-limit counter and sets its TTL in one
/// round-trip, self-healing a counter left without a TTL by a crash between
/// a separate INCR and EXPIRE (or by the pre-fix two-call version of this
/// check). Same pattern as `middleware::quota::INCREMENT_WITH_EXPIRY_SCRIPT`.
const RATE_LIMIT_INCREMENT_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
local healed = 0
if current == 1 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
elseif redis.call('TTL', KEYS[1]) < 0 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
    healed = 1
end
return {current, healed}
"#;

/// How long a half-open probe lease is held, in milliseconds — long enough
/// to cover one delivery attempt's HTTP timeout plus margin, short enough
/// that a crashed probe holder doesn't wedge the breaker in "no one may
/// probe" for long.
const CB_PROBE_LEASE_MS: i64 = 30_000;

/// Result of checking an endpoint's circuit breaker state.
#[derive(Debug, PartialEq, Eq)]
enum CircuitDecision {
    /// Breaker is closed (or was never tripped); all deliveries proceed.
    Closed,
    /// Breaker is open and still within its reset timeout; all deliveries
    /// for this endpoint should be rescheduled.
    Open,
    /// Breaker is open but past its reset timeout, and this caller acquired
    /// the half-open probe lease: exactly one delivery should be let
    /// through as the probe; the rest should be rescheduled. If the probe
    /// succeeds the breaker resets; if it fails, `opened_at` is refreshed
    /// and the endpoint stays open for another full reset window.
    HalfOpenProbe,
}

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
    pub attempt_history: Option<serde_json::Value>,
    pub claimed_at: Option<chrono::DateTime<Utc>>,
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
    /// Uses `FOR UPDATE SKIP LOCKED` in a CTE to claim rows atomically so
    /// concurrent replicas never deliver the same event twice.
    /// Also reclaims stuck `in_progress` rows past `CLAIM_TIMEOUT_SECS`.
    pub async fn process_pending(&self) -> anyhow::Result<()> {
        let reclaim_cutoff = Utc::now() - chrono::Duration::seconds(CLAIM_TIMEOUT_SECS);

        // Atomic claim via a CTE: the inner SELECT … FOR UPDATE SKIP LOCKED
        // picks rows that are not already locked by another transaction, then
        // the outer UPDATE claims them and returns the joined result.
        let deliveries: Vec<WebhookDelivery> = sqlx::query_as(
            r#"
            WITH candidate AS (
                SELECT id FROM webhook_deliveries
                WHERE (status = 'pending'
                   AND (next_attempt_at IS NULL OR next_attempt_at <= NOW()))
                   OR (status = 'in_progress' AND claimed_at <= $1)
                ORDER BY created_at
                LIMIT 100
                FOR UPDATE SKIP LOCKED
            )
            UPDATE webhook_deliveries wd
            SET status   = 'in_progress',
                claimed_at = NOW()
            FROM webhook_endpoints we, candidate c
            WHERE wd.id = c.id
              AND wd.endpoint_id = we.id
              AND we.enabled = true
            RETURNING wd.*, we.max_delivery_rate
            "#,
        )
        .bind(reclaim_cutoff)
        .fetch_all(&self.pool)
        .await?;

        if deliveries.is_empty() {
            return Ok(());
        }

        // Group claimed deliveries by endpoint for circuit-breaker checks.
        let mut by_endpoint: HashMap<Uuid, Vec<WebhookDelivery>> = HashMap::new();
        for d in deliveries {
            by_endpoint.entry(d.endpoint_id).or_default().push(d);
        }

        let endpoint_ids: Vec<Uuid> = by_endpoint.keys().copied().collect();

        let endpoints: Vec<WebhookEndpoint> =
            sqlx::query_as("SELECT * FROM webhook_endpoints WHERE id = ANY($1) AND enabled = true")
                .bind(&endpoint_ids)
                .fetch_all(&self.pool)
                .await?;

        let endpoint_map: HashMap<Uuid, WebhookEndpoint> =
            endpoints.into_iter().map(|ep| (ep.id, ep)).collect();

        for ep_id in &endpoint_ids {
            if !endpoint_map.contains_key(ep_id) {
                continue;
            }

            let decision = self
                .circuit_breaker_decision(ep_id)
                .await
                .unwrap_or(CircuitDecision::Closed);

            match decision {
                CircuitDecision::Closed => {}
                CircuitDecision::Open => {
                    if let Some(deliveries) = by_endpoint.get(ep_id) {
                        self.reschedule_after_breaker_block(ep_id, deliveries)
                            .await?;
                    }
                    by_endpoint.remove(ep_id);
                }
                CircuitDecision::HalfOpenProbe => {
                    // Let exactly one delivery through as the probe; every
                    // other delivery queued for this endpoint gets
                    // rescheduled rather than joining a synchronized burst
                    // the instant the reset timeout elapses.
                    if let Some(deliveries) = by_endpoint.get_mut(ep_id) {
                        if deliveries.len() > 1 {
                            let rest = deliveries.split_off(1);
                            self.reschedule_after_breaker_block(ep_id, &rest).await?;
                        }
                    }
                }
            }
        }

        // Flatten remaining (non-blocked) deliveries
        let remaining: Vec<WebhookDelivery> = by_endpoint.into_values().flatten().collect();

        if remaining.is_empty() {
            return Ok(());
        }

        tracing::info!(
            delivery_count = remaining.len(),
            endpoint_count = endpoint_map.len(),
            "Webhook dispatcher processing claimed deliveries"
        );

        stream::iter(remaining)
            .map(|delivery| {
                let dispatcher = self.clone();
                let endpoint_map = endpoint_map.clone();
                async move {
                    let start = std::time::Instant::now();
                    if let Err(e) = dispatcher
                        .attempt_delivery_with_endpoint(&delivery, &endpoint_map)
                        .await
                    {
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

        // INCR + conditional EXPIRE as a single atomic Lua script (same
        // pattern as middleware/quota.rs's INCREMENT_WITH_EXPIRY_SCRIPT):
        // if this process died between a separate INCR and EXPIRE, the key
        // would be left with no TTL and never reset, permanently pinning
        // this endpoint's rate limit. The TTL < 0 check also self-heals any
        // counter already left in that state by the pre-fix two-call version.
        let (current_count, healed): (i32, i32) = Script::new(RATE_LIMIT_INCREMENT_SCRIPT)
            .key(&key)
            .arg(60)
            .invoke_async(&mut conn)
            .await?;
        if healed == 1 {
            crate::metrics::webhook_rate_limit_self_healed_total().add(1, &[]);
            tracing::warn!(
                endpoint_id = %endpoint_id,
                "Rate limit counter found without a TTL and self-healed"
            );
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

    async fn attempt_delivery_with_endpoint(
        &self,
        delivery: &WebhookDelivery,
        endpoint_map: &HashMap<Uuid, WebhookEndpoint>,
    ) -> anyhow::Result<()> {
        // Check rate limit first
        if !self
            .check_rate_limit(delivery.endpoint_id, delivery.max_delivery_rate)
            .await?
        {
            // Rate limit exceeded, delay this delivery to next cycle
            let next_cycle = Utc::now() + chrono::Duration::seconds(30);
            sqlx::query(
                r#"
                UPDATE webhook_deliveries
                SET status = 'pending',
                    claimed_at = NULL,
                    next_attempt_at = $1
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

        let endpoint = match endpoint_map.get(&delivery.endpoint_id) {
            Some(ep) => ep,
            None => {
                tracing::warn!(
                    delivery_id = %delivery.id,
                    endpoint_id = %delivery.endpoint_id,
                    "Endpoint not found in batch-loaded map"
                );
                // Release claim so it can be picked up next cycle
                sqlx::query(
                    "UPDATE webhook_deliveries SET status = 'pending', claimed_at = NULL WHERE id = $1",
                )
                .bind(delivery.id)
                .execute(&self.pool)
                .await?;
                return Ok(());
            }
        };

        let started = std::time::Instant::now();
        let result = self.send_webhook(delivery, endpoint).await;
        crate::metrics::webhook_delivery_duration_ms()
            .record(started.elapsed().as_secs_f64() * 1000.0, &[]);

        let outcome = if matches!(result, Ok(true)) {
            "success"
        } else {
            "failure"
        };
        crate::metrics::webhook_delivery_total().add(
            1,
            &[
                opentelemetry::KeyValue::new("outcome", outcome),
                opentelemetry::KeyValue::new("endpoint_id", delivery.endpoint_id.to_string()),
            ],
        );

        // Record circuit breaker outcome based on whether the delivery
        // itself succeeded or failed (not just whether send_webhook ran
        // without a Rust-level error — a 4xx/5xx response is a business
        // failure that must trip the breaker, previously it didn't).
        match &result {
            Ok(true) => {
                let _ = self.circuit_breaker_succeeded(&delivery.endpoint_id).await;
            }
            Ok(false) => {
                let _ = self.circuit_breaker_failed(&delivery.endpoint_id).await;
            }
            Err(_) => {
                // A genuine Rust/DB-level error while recording the attempt
                // isn't information about the endpoint's health; leave the
                // breaker state untouched either way.
            }
        }

        result.map(|_| ())
    }

    /// Build an attempt-history entry and append it to the delivery's JSONB column.
    async fn append_attempt_history(
        &self,
        delivery_id: Uuid,
        attempt: i32,
        attempted_at: chrono::DateTime<Utc>,
        response_status: Option<i32>,
        response_body: Option<String>,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let entry = serde_json::json!({
            "attempt": attempt,
            "attempted_at": attempted_at.to_rfc3339(),
            "response_status": response_status,
            "response_body": response_body,
            "error": error,
        });

        sqlx::query(
            r#"
            UPDATE webhook_deliveries
            SET attempt_history = COALESCE(attempt_history, '[]'::jsonb) || $1::jsonb
            WHERE id = $2
            "#,
        )
        .bind(entry.to_string())
        .bind(delivery_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Sends the HTTP request and persists the outcome. Returns `Ok(true)`
    /// if the delivery succeeded (2xx response), `Ok(false)` if it failed
    /// (non-2xx response or a transport error — both already handled via
    /// `handle_failure` before returning). `Err` is reserved for a genuine
    /// Rust/DB-level failure while recording the attempt, which is distinct
    /// from "the webhook delivery itself failed" and should not be read as
    /// information about the endpoint's health by callers gating a circuit
    /// breaker on this result.
    async fn send_webhook(
        &self,
        delivery: &WebhookDelivery,
        endpoint: &WebhookEndpoint,
    ) -> anyhow::Result<bool> {
        let body = serde_json::to_string(&delivery.payload)?;

        // Extract timestamp from payload (OutgoingPayload includes timestamp field)
        let timestamp = delivery
            .payload
            .get("timestamp")
            .and_then(|ts| ts.as_str())
            .map(|ts| ts.to_string())
            .unwrap_or_else(|| Utc::now().to_rfc3339());

        let signature = sign_payload_with_version(&endpoint.secret, &timestamp, &body);

        // Get trace_id from transaction if available
        let trace_id: Option<String> =
            sqlx::query_scalar("SELECT trace_id FROM transactions WHERE id = $1")
                .bind(delivery.transaction_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten();

        let mut request = self
            .http
            .post(&endpoint.url)
            .header("Content-Type", "application/json")
            .header("X-Webhook-Signature", &signature)
            .header("X-Webhook-Timestamp", &timestamp)
            .header("X-Webhook-Event", &delivery.event_type);

        if let Some(trace_id) = trace_id {
            request = request.header("X-Trace-Id", trace_id);
        }

        let response = request.body(body).send().await;

        let new_attempt_count = delivery.attempt_count + 1;
        let now = Utc::now();

        match response {
            Ok(resp) => {
                let status_code = resp.status().as_u16() as i32;
                let resp_body = resp.text().await.unwrap_or_default();
                let success = (200..300).contains(&(status_code as u16));

                // Record attempt history
                self.append_attempt_history(
                    delivery.id,
                    new_attempt_count,
                    now,
                    Some(status_code),
                    Some(resp_body.clone()),
                    None,
                )
                .await?;

                if success {
                    sqlx::query(
                        r#"
                        UPDATE webhook_deliveries
                        SET status = 'delivered',
                            attempt_count = $1,
                            last_attempt_at = $2,
                            response_status = $3,
                            response_body = $4,
                            claimed_at = NULL
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
                    Ok(true)
                } else {
                    self.handle_failure(
                        delivery,
                        new_attempt_count,
                        now,
                        Some(status_code),
                        Some(resp_body),
                    )
                    .await?;
                    Ok(false)
                }
            }
            Err(e) => {
                let err_msg = e.to_string();

                // Record attempt history
                self.append_attempt_history(
                    delivery.id,
                    new_attempt_count,
                    now,
                    None,
                    None,
                    Some(err_msg.clone()),
                )
                .await?;

                self.handle_failure(delivery, new_attempt_count, now, None, Some(err_msg))
                    .await?;
                Ok(false)
            }
        }
    }

    #[allow(dead_code)]
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
                SET status = 'pending',
                    claimed_at = NULL,
                    next_attempt_at = $1
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

        self.send_webhook(delivery, &endpoint).await.map(|_| ())
    }

    /// Handle a failed delivery attempt.
    ///
    /// * If `attempt_count < MAX_ATTEMPTS`: schedule a retry with exponential
    ///   backoff and keep the status as `pending`.
    /// * If `attempt_count >= MAX_ATTEMPTS`: move the delivery to the DLQ table
    ///   with full attempt history, set status to `failed`.
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
                endpoint_id = %delivery.endpoint_id,
                attempt_count = attempt_count,
                "Webhook delivery exhausted, routing to DLQ"
            );
            ("failed", None)
        } else {
            let base_delay = BASE_DELAY_SECS * (1_i64 << attempt_count);
            let delay = crate::utils::retry::apply_jitter(base_delay as u64) as i64;
            let next = now + chrono::Duration::seconds(delay);
            tracing::warn!(
                delivery_id = %delivery.id,
                attempt = attempt_count,
                next_retry_in_secs = delay,
                "Webhook delivery failed, scheduling retry"
            );
            ("pending", Some(next))
        };

        // Update delivery record
        sqlx::query(
            r#"
            UPDATE webhook_deliveries
            SET status = $1,
                attempt_count = $2,
                last_attempt_at = $3,
                next_attempt_at = $4,
                response_status = $5,
                response_body = $6,
                claimed_at = NULL
            WHERE id = $7
            "#,
        )
        .bind(new_status)
        .bind(attempt_count)
        .bind(now)
        .bind(next_attempt_at)
        .bind(response_status)
        .bind(response_body.clone())
        .bind(delivery.id)
        .execute(&self.pool)
        .await?;

        // Route to DLQ on exhaustion
        if attempt_count >= MAX_ATTEMPTS {
            self.route_to_dlq(delivery, attempt_count, response_status, response_body)
                .await?;
        }

        Ok(())
    }

    /// Insert an exhausted delivery into the DLQ table with the full attempt
    /// history so operators can inspect and replay.
    async fn route_to_dlq(
        &self,
        delivery: &WebhookDelivery,
        attempt_count: i32,
        response_status: Option<i32>,
        response_body: Option<String>,
    ) -> anyhow::Result<()> {
        // Fetch the latest attempt_history persisted by append_attempt_history
        let history: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT attempt_history FROM webhook_deliveries WHERE id = $1")
                .bind(delivery.id)
                .fetch_optional(&self.pool)
                .await?;

        sqlx::query(
            r#"
            INSERT INTO webhook_delivery_dlq
                (delivery_id, endpoint_id, transaction_id, event_type,
                 payload, attempt_history, attempt_count,
                 last_response_status, last_response_body, last_error)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (delivery_id) DO NOTHING
            "#,
        )
        .bind(delivery.id)
        .bind(delivery.endpoint_id)
        .bind(delivery.transaction_id)
        .bind(&delivery.event_type)
        .bind(&delivery.payload)
        .bind(history.unwrap_or(serde_json::Value::Array(vec![])))
        .bind(attempt_count)
        .bind(response_status)
        .bind(response_body)
        .bind(None::<String>)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            delivery_id = %delivery.id,
            endpoint_id = %delivery.endpoint_id,
            "Webhook delivery moved to DLQ"
        );

        Ok(())
    }

    // -------------------------------------------------------------------
    // Circuit breaker helpers (Redis-backed)
    // -------------------------------------------------------------------

    /// Reschedule deliveries blocked by an open/half-open-but-not-probing
    /// circuit breaker, without consuming an attempt. Each delivery gets an
    /// independently jittered `next_attempt_at` around the reset timeout
    /// instead of the identical instant, so when the breaker does reset,
    /// the endpoint sees a spread-out trickle of retries rather than every
    /// queued delivery for it firing in the same `process_pending` tick.
    async fn reschedule_after_breaker_block(
        &self,
        endpoint_id: &Uuid,
        deliveries: &[WebhookDelivery],
    ) -> anyhow::Result<()> {
        for d in deliveries {
            let jittered_secs =
                crate::utils::retry::apply_jitter(CB_RESET_TIMEOUT_SECS as u64) as i64;
            let next_cycle = Utc::now() + chrono::Duration::seconds(jittered_secs);
            sqlx::query(
                r#"
                UPDATE webhook_deliveries
                SET status        = 'pending',
                    claimed_at    = NULL,
                    next_attempt_at = $1
                WHERE id = $2
                "#,
            )
            .bind(next_cycle)
            .bind(d.id)
            .execute(&self.pool)
            .await?;

            tracing::warn!(
                delivery_id = %d.id,
                endpoint_id = %endpoint_id,
                next_attempt_in_secs = jittered_secs,
                "Circuit breaker open, rescheduled delivery without consuming attempt"
            );
        }
        Ok(())
    }

    /// Check the circuit breaker state for this endpoint and, if it's past
    /// its reset timeout, try to acquire the half-open probe lease.
    ///
    /// There was previously no half-open gating anywhere in this codebase
    /// (the separate `CircuitBreaker` type in `circuit_breaker.rs` doesn't
    /// implement it either — its "half-open" is an in-process mutex guard
    /// dropped before the call runs, with no lease of any kind, and its
    /// Redis persistence is write-only/never read back). This lease
    /// (`SET NX PX`) is what actually makes "exactly one probe through"
    /// true, including across multiple instances sharing the same Redis.
    async fn circuit_breaker_decision(
        &self,
        endpoint_id: &Uuid,
    ) -> anyhow::Result<CircuitDecision> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = format!("webhook_cb:{endpoint_id}");
        let data: Option<String> = conn.get(&key).await?;

        let Some(json) = data else {
            return Ok(CircuitDecision::Closed);
        };
        let state: serde_json::Value = serde_json::from_str(&json)?;
        if state["state"] != "open" {
            return Ok(CircuitDecision::Closed);
        }

        let opened_at = state["opened_at"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(Utc::now());
        let elapsed = Utc::now() - opened_at;
        if elapsed < chrono::Duration::seconds(CB_RESET_TIMEOUT_SECS) {
            return Ok(CircuitDecision::Open);
        }

        let probe_key = format!("webhook_cb_probe:{endpoint_id}");
        let acquired: Option<String> = redis::cmd("SET")
            .arg(&probe_key)
            .arg("1")
            .arg("NX")
            .arg("PX")
            .arg(CB_PROBE_LEASE_MS)
            .query_async(&mut conn)
            .await?;

        if acquired.is_some() {
            crate::metrics::webhook_circuit_breaker_transitions_total().add(
                1,
                &[opentelemetry::KeyValue::new("transition", "probe_sent")],
            );
            Ok(CircuitDecision::HalfOpenProbe)
        } else {
            // Someone else already holds the probe lease for this window.
            crate::metrics::webhook_circuit_breaker_transitions_total().add(
                1,
                &[opentelemetry::KeyValue::new("transition", "probe_blocked")],
            );
            Ok(CircuitDecision::Open)
        }
    }

    /// Record a successful delivery — reset the circuit breaker and release
    /// the probe lease so a future trip can probe again immediately rather
    /// than waiting out a stale lease.
    async fn circuit_breaker_succeeded(&self, endpoint_id: &Uuid) -> anyhow::Result<()> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = format!("webhook_cb:{endpoint_id}");
        let probe_key = format!("webhook_cb_probe:{endpoint_id}");
        let deleted: i32 = redis::cmd("DEL")
            .arg(&key)
            .arg(&probe_key)
            .query_async(&mut conn)
            .await?;
        if deleted > 0 {
            crate::metrics::webhook_circuit_breaker_transitions_total()
                .add(1, &[opentelemetry::KeyValue::new("transition", "closed")]);
        }
        Ok(())
    }

    /// Record a failed delivery — may trip the circuit breaker open.
    async fn circuit_breaker_failed(&self, endpoint_id: &Uuid) -> anyhow::Result<()> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = format!("webhook_cb:{endpoint_id}");

        // Use a Redis Lua script for atomic read-modify-write
        let script = Script::new(
            r#"
            local data = redis.call('GET', KEYS[1])
            local state
            if data then
                state = cjson.decode(data)
                state.failure_count = (state.failure_count or 0) + 1
            else
                state = { state = 'closed', failure_count = 1, opened_at = nil, last_error = nil }
            end
            state.last_error = ARGV[1]
            if state.failure_count >= tonumber(ARGV[2]) then
                state.state = 'open'
                state.opened_at = ARGV[3]
            else
                state.state = 'closed'
            end
            redis.call('SETEX', KEYS[1], ARGV[4], cjson.encode(state))
            return {state.failure_count, state.state}
            "#,
        );

        let (_, new_state): (i32, String) = script
            .key(&key)
            .arg("delivery failed")
            .arg(CB_FAILURE_THRESHOLD)
            .arg(Utc::now().to_rfc3339())
            .arg(CB_RESET_TIMEOUT_SECS)
            .invoke_async(&mut conn)
            .await?;

        if new_state == "open" {
            crate::metrics::webhook_circuit_breaker_transitions_total()
                .add(1, &[opentelemetry::KeyValue::new("transition", "opened")]);
        }

        Ok(())
    }

    // -------------------------------------------------------------------
    // DLQ replay
    // -------------------------------------------------------------------

    /// Replay a webhook delivery from the DLQ back into the delivery table.
    /// The delivery is re-enqueued as a fresh `pending` row with the original
    /// payload and a reset attempt counter. Returns the new delivery id.
    pub async fn replay_from_dlq(&self, dlq_id: Uuid) -> anyhow::Result<Uuid> {
        let dlq_row = sqlx::query(
            r#"
            SELECT delivery_id, endpoint_id, transaction_id, event_type, payload
            FROM webhook_delivery_dlq
            WHERE id = $1
            "#,
        )
        .bind(dlq_id)
        .fetch_optional(&self.pool)
        .await?;

        let (delivery_id, endpoint_id, transaction_id, event_type, payload) = match dlq_row {
            Some(row) => (
                row.try_get::<Uuid, _>("delivery_id")?,
                row.try_get::<Uuid, _>("endpoint_id")?,
                row.try_get::<Uuid, _>("transaction_id")?,
                row.try_get::<String, _>("event_type")?,
                row.try_get::<serde_json::Value, _>("payload")?,
            ),
            None => anyhow::bail!("DLQ entry {dlq_id} not found"),
        };

        // Re-insert into deliveries (ON CONFLICT DO NOTHING means if the
        // original row still exists we reuse it)
        let new_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO webhook_deliveries
                (endpoint_id, transaction_id, event_type, payload, status, next_attempt_at, attempt_history)
            VALUES ($1, $2, $3, $4, 'pending', NOW(), '[]'::jsonb)
            ON CONFLICT (endpoint_id, transaction_id, event_type)
            DO UPDATE SET status = 'pending',
                          next_attempt_at = NOW(),
                          attempt_count = 0,
                          response_status = NULL,
                          response_body = NULL,
                          attempt_history = '[]'::jsonb,
                          claimed_at = NULL
            RETURNING id
            "#,
        )
        .bind(endpoint_id)
        .bind(transaction_id)
        .bind(&event_type)
        .bind(&payload)
        .fetch_one(&self.pool)
        .await?;

        // Increment replay counter on DLQ entry
        sqlx::query(
            r#"
            UPDATE webhook_delivery_dlq
            SET replay_count = replay_count + 1,
                replayed_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(dlq_id)
        .execute(&self.pool)
        .await?;

        tracing::info!(
            dlq_id = %dlq_id,
            delivery_id = %delivery_id,
            new_delivery_id = %new_id,
            "Webhook delivery replayed from DLQ"
        );

        Ok(new_id)
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
            (successes as f64 / total as f64) * 100.0
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

    // ── Part E regression tests ────────────────────────────────────────────

    fn test_redis_url() -> String {
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
    }

    fn make_dispatcher(redis_url: &str) -> WebhookDispatcher {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap();
        WebhookDispatcher::new(pool, redis_url).unwrap()
    }

    /// Part E.3 regression: a rate-limit counter left without a TTL (e.g. by
    /// a crash between a separate INCR and EXPIRE, which is exactly what the
    /// pre-fix two-call version of check_rate_limit could produce) must
    /// self-heal on the next check rather than remaining permanently
    /// rate-limited forever.
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn test_rate_limit_self_heals_counter_left_without_ttl() {
        let redis_url = test_redis_url();
        let dispatcher = make_dispatcher(&redis_url);
        let endpoint_id = Uuid::new_v4();

        let client = redis::Client::open(redis_url).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let key = format!("webhook_rate:{endpoint_id}");

        // Directly construct the "crashed between INCR and EXPIRE" state.
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(999)
            .query_async(&mut conn)
            .await
            .unwrap();
        let ttl_before: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl_before, -1, "test precondition: key should have no TTL");

        // max_rate well above the pre-existing count — we're testing TTL
        // self-heal here, not the limit threshold itself.
        let allowed = dispatcher
            .check_rate_limit(endpoint_id, 1000)
            .await
            .unwrap();
        assert!(allowed);

        let ttl_after: i64 = redis::cmd("TTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(
            ttl_after > 0,
            "expected the self-heal to set a TTL on the previously-stuck key, got {ttl_after}"
        );

        let _: () = redis::cmd("DEL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
    }

    /// Part E.2 regression: once a breaker is past its reset timeout,
    /// exactly one concurrent caller should be granted the half-open probe
    /// lease; every other concurrent caller for the same endpoint must see
    /// `Open`, not also `HalfOpenProbe` — otherwise every queued delivery
    /// for that endpoint fires as soon as the timeout elapses instead of a
    /// single probe deciding whether the endpoint has actually recovered.
    #[ignore = "Requires Redis"]
    #[tokio::test]
    async fn test_half_open_probe_lease_grants_exactly_one_probe() {
        let redis_url = test_redis_url();
        let dispatcher = make_dispatcher(&redis_url);
        let endpoint_id = Uuid::new_v4();

        let client = redis::Client::open(redis_url.clone()).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cb_key = format!("webhook_cb:{endpoint_id}");
        let probe_key = format!("webhook_cb_probe:{endpoint_id}");
        let _: () = redis::cmd("DEL")
            .arg(&cb_key)
            .arg(&probe_key)
            .query_async(&mut conn)
            .await
            .unwrap();

        // Directly construct "open, past its reset timeout" state rather
        // than waiting out CB_RESET_TIMEOUT_SECS in real time.
        let opened_at = Utc::now() - chrono::Duration::seconds(CB_RESET_TIMEOUT_SECS + 5);
        let state = serde_json::json!({
            "state": "open",
            "failure_count": CB_FAILURE_THRESHOLD,
            "opened_at": opened_at.to_rfc3339(),
            "last_error": "test",
        });
        let _: () = redis::cmd("SET")
            .arg(&cb_key)
            .arg(state.to_string())
            .query_async(&mut conn)
            .await
            .unwrap();

        let dispatcher_a = dispatcher.clone();
        let dispatcher_b = make_dispatcher(&redis_url);
        let (decision_a, decision_b) = tokio::join!(
            dispatcher_a.circuit_breaker_decision(&endpoint_id),
            dispatcher_b.circuit_breaker_decision(&endpoint_id),
        );
        let decision_a = decision_a.unwrap();
        let decision_b = decision_b.unwrap();

        let probes = [&decision_a, &decision_b]
            .iter()
            .filter(|d| ***d == CircuitDecision::HalfOpenProbe)
            .count();
        assert_eq!(
            probes, 1,
            "expected exactly one concurrent caller to win the probe lease, got a={:?} b={:?}",
            decision_a, decision_b
        );
        let blocked = [&decision_a, &decision_b]
            .iter()
            .filter(|d| ***d == CircuitDecision::Open)
            .count();
        assert_eq!(blocked, 1, "expected the loser to be told Open, not Closed");

        let _: () = redis::cmd("DEL")
            .arg(&cb_key)
            .arg(&probe_key)
            .query_async(&mut conn)
            .await
            .unwrap();
    }
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

    use sqlx::Row;
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
