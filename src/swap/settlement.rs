//! Settlement of executed swap orders.
//!
//! Flow: `executed` -> `settlement_pending` -> `settled` (or `failed` with a
//! reason code on definitive rejection). Submission is separated from
//! confirmation: the transaction hash (the external reference) is persisted
//! *before* the payout is submitted, so a crash or an ambiguous Horizon
//! response is recovered by looking the hash up - never by re-submitting.
//! Idempotency key: `swap-settle:{order_id}:{attempt}`.

use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

/// Payout to be delivered on the destination side.
#[derive(Debug, Clone)]
pub struct SettlementRequest {
    pub order_id: Uuid,
    pub attempt: i32,
    pub idempotency_key: String,
    /// Transaction hash of the signed envelope (known before submission).
    pub tx_hash: String,
    /// Signed transaction envelope (base64 XDR). Never logged.
    pub envelope_xdr: String,
}

/// Builds and signs the payout envelope. Implementations must read keys from
/// env or a secrets manager and must never log them.
#[async_trait]
pub trait PayoutSigner: Send + Sync {
    async fn sign(
        &self,
        order: &SwapOrder,
        attempt: i32,
        idempotency_key: &str,
    ) -> Result<SignedPayout, String>;
}

#[derive(Debug, Clone)]
pub struct SignedPayout {
    pub tx_hash: String,
    pub envelope_xdr: String,
}

/// Normalized external status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalStatus {
    Confirmed { raw: String },
    /// Unknown / not yet visible. Never treated as failure.
    Pending { raw: String },
    Rejected { raw: String, reason_code: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    /// Timeout, network error or 5xx: outcome unknown, not failed.
    Unknown(String),
    /// Definitive rejection; payout can never succeed under this hash.
    Rejected { reason_code: String, raw: String },
}

#[async_trait]
pub trait SettlementAdapter: Send + Sync {
    /// Submit the payout. `Ok` means accepted (or already known) by the network.
    async fn submit(&self, request: &SettlementRequest) -> Result<String, AdapterError>;
    /// Look up the external status of a previously recorded reference.
    async fn status(&self, reference: &str) -> Result<ExternalStatus, AdapterError>;
}

/// Submits Stellar payouts through Horizon and polls for confirmation.
pub struct HorizonSettlementAdapter {
    base_url: String,
    client: reqwest::Client,
}

impl HorizonSettlementAdapter {
    pub fn new(base_url: String) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        }
    }
}

#[async_trait]
impl SettlementAdapter for HorizonSettlementAdapter {
    async fn submit(&self, request: &SettlementRequest) -> Result<String, AdapterError> {
        let resp = self
            .client
            .post(format!("{}/transactions", self.base_url))
            .form(&[("tx", request.envelope_xdr.as_str())])
            .send()
            .await
            .map_err(|e| AdapterError::Unknown(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(request.tx_hash.clone());
        }
        if status.is_server_error() || status.as_u16() == 408 || status.as_u16() == 429 {
            return Err(AdapterError::Unknown(format!("horizon {status}")));
        }
        // 4xx: tx_bad_seq, tx_insufficient_balance, op_no_trust, ... are definitive.
        let body = resp.text().await.unwrap_or_default();
        let raw: String = body.chars().take(512).collect();
        let reason_code = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v["extras"]["result_codes"]["transaction"]
                    .as_str()
                    .map(String::from)
            })
            .unwrap_or_else(|| format!("http_{}", status.as_u16()));
        Err(AdapterError::Rejected { reason_code, raw })
    }

    async fn status(&self, reference: &str) -> Result<ExternalStatus, AdapterError> {
        let resp = self
            .client
            .get(format!("{}/transactions/{}", self.base_url, reference))
            .send()
            .await
            .map_err(|e| AdapterError::Unknown(e.to_string()))?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Ok(ExternalStatus::Pending {
                raw: "not_found".into(),
            });
        }
        if !status.is_success() {
            return Err(AdapterError::Unknown(format!("horizon {status}")));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AdapterError::Unknown(e.to_string()))?;
        let raw = v["successful"].to_string();
        if v["successful"].as_bool() == Some(true) {
            Ok(ExternalStatus::Confirmed { raw })
        } else {
            Ok(ExternalStatus::Rejected {
                raw,
                reason_code: "tx_failed".into(),
            })
        }
    }
}

/// Retry/backoff and reconciliation thresholds, read from env.
#[derive(Debug, Clone)]
pub struct SettlementConfig {
    pub max_submit_attempts: u32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    /// `SettlementPending` older than this is reconciled.
    pub stuck_threshold: Duration,
    pub reconcile_interval: Duration,
}

impl Default for SettlementConfig {
    fn default() -> Self {
        Self {
            max_submit_attempts: 5,
            base_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            stuck_threshold: Duration::from_secs(300),
            reconcile_interval: Duration::from_secs(60),
        }
    }
}

impl SettlementConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        let secs = |k: &str, def: Duration| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(def)
        };
        Self {
            max_submit_attempts: std::env::var("SETTLEMENT_MAX_SUBMIT_ATTEMPTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d.max_submit_attempts),
            base_backoff: d.base_backoff,
            max_backoff: secs("SETTLEMENT_MAX_BACKOFF_SECS", d.max_backoff),
            stuck_threshold: secs("SETTLEMENT_STUCK_THRESHOLD_SECS", d.stuck_threshold),
            reconcile_interval: secs("SETTLEMENT_RECONCILE_INTERVAL_SECS", d.reconcile_interval),
        }
    }

    /// Exponential backoff for the given zero-based retry number.
    pub fn backoff(&self, retry: u32) -> Duration {
        let f = 1u32.checked_shl(retry.min(16)).unwrap_or(u32::MAX);
        self.base_backoff.saturating_mul(f).min(self.max_backoff)
    }
}

/// Hook invoked for orders stuck in `SettlementPending` beyond the threshold.
#[async_trait]
pub trait StuckAlert: Send + Sync {
    async fn order_stuck(&self, order_id: Uuid, tx_hash: Option<&str>);
}

/// Default hook: emits an error-level log line that observability alerts on.
pub struct LogStuckAlert;

#[async_trait]
impl StuckAlert for LogStuckAlert {
    async fn order_stuck(&self, order_id: Uuid, tx_hash: Option<&str>) {
        tracing::error!(
            %order_id,
            ?tx_hash,
            alert = "swap_settlement_stuck",
            "swap order stuck in settlement_pending"
        );
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapOrder {
    pub id: Uuid,
    pub status: String,
    pub destination_account: String,
    pub output_asset: String,
    pub output_amount: sqlx::types::BigDecimal,
    pub settlement_attempt: i32,
    pub settlement_tx_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementOutcome {
    Settled,
    /// Submitted but unconfirmed; reconciliation will resolve it.
    Pending,
    Failed { reason_code: String },
}

pub fn idempotency_key(order_id: Uuid, attempt: i32) -> String {
    format!("swap-settle:{order_id}:{attempt}")
}

const ORDER_COLUMNS: &str = "id, status, destination_account, output_asset, output_amount, \
                             settlement_attempt, settlement_tx_hash";

pub struct SettlementService<A: SettlementAdapter> {
    pool: PgPool,
    adapter: A,
    config: SettlementConfig,
}

impl<A: SettlementAdapter> SettlementService<A> {
    pub fn new(pool: PgPool, adapter: A, config: SettlementConfig) -> Self {
        Self {
            pool,
            adapter,
            config,
        }
    }

    async fn event(
        &self,
        order_id: Uuid,
        event_type: &str,
        attempt: Option<i32>,
        reference: Option<&str>,
        raw: Option<&str>,
        detail: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO swap_events (order_id, event_type, attempt, external_reference, raw_status, detail)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(order_id)
        .bind(event_type)
        .bind(attempt)
        .bind(reference)
        .bind(raw)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Handoff from execution: settle an `executed` order. If a reference
    /// already exists the order is reconciled instead of re-submitted.
    pub async fn settle(
        &self,
        order_id: Uuid,
        signer: &dyn PayoutSigner,
    ) -> Result<SettlementOutcome, sqlx::Error> {
        let order: SwapOrder =
            sqlx::query_as(&format!("SELECT {ORDER_COLUMNS} FROM swap_orders WHERE id = $1"))
                .bind(order_id)
                .fetch_one(&self.pool)
                .await?;

        match order.status.as_str() {
            "settled" => return Ok(SettlementOutcome::Settled),
            "failed" => {
                return Ok(SettlementOutcome::Failed {
                    reason_code: "already_failed".into(),
                })
            }
            _ => {}
        }
        if order.settlement_tx_hash.is_some() {
            return self.reconcile_order(&order).await;
        }

        // Claim the attempt atomically: only one caller moves executed -> pending.
        let claimed: Option<(i32,)> = sqlx::query_as(
            "UPDATE swap_orders
             SET status = 'settlement_pending', settlement_attempt = settlement_attempt + 1,
                 settlement_started_at = NOW(), updated_at = NOW()
             WHERE id = $1 AND status = 'executed' AND settlement_tx_hash IS NULL
             RETURNING settlement_attempt",
        )
        .bind(order_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some((attempt,)) = claimed else {
            return Ok(SettlementOutcome::Pending);
        };

        let key = idempotency_key(order_id, attempt);
        let signed = match signer.sign(&order, attempt, &key).await {
            Ok(s) => s,
            Err(e) => {
                self.fail(order_id, attempt, "signing_failed", &e).await?;
                return Ok(SettlementOutcome::Failed {
                    reason_code: "signing_failed".into(),
                });
            }
        };

        // Persist the reference BEFORE submitting (crash recovery).
        sqlx::query(
            "UPDATE swap_orders SET settlement_idempotency_key = $2, settlement_tx_hash = $3,
                    updated_at = NOW() WHERE id = $1",
        )
        .bind(order_id)
        .bind(&key)
        .bind(&signed.tx_hash)
        .execute(&self.pool)
        .await?;
        self.event(
            order_id,
            "settlement_attempt",
            Some(attempt),
            Some(&signed.tx_hash),
            None,
            json!({ "idempotency_key": key }),
        )
        .await?;

        let request = SettlementRequest {
            order_id,
            attempt,
            idempotency_key: key,
            tx_hash: signed.tx_hash,
            envelope_xdr: signed.envelope_xdr,
        };

        // Re-sending the same signed envelope is safe: same hash, so the
        // network applies it at most once.
        let mut retry = 0;
        loop {
            match self.adapter.submit(&request).await {
                Ok(_) => {
                    self.event(
                        order_id,
                        "settlement_submitted",
                        Some(attempt),
                        Some(&request.tx_hash),
                        None,
                        json!({}),
                    )
                    .await?;
                    break;
                }
                Err(AdapterError::Rejected { reason_code, raw }) => {
                    self.fail(order_id, attempt, &reason_code, &raw).await?;
                    return Ok(SettlementOutcome::Failed { reason_code });
                }
                Err(AdapterError::Unknown(msg)) => {
                    retry += 1;
                    self.event(
                        order_id,
                        "settlement_submit_unknown",
                        Some(attempt),
                        Some(&request.tx_hash),
                        None,
                        json!({ "error": msg, "retry": retry }),
                    )
                    .await?;
                    if retry >= self.config.max_submit_attempts {
                        // Unknown, not failed: reconciliation looks up the hash.
                        return Ok(SettlementOutcome::Pending);
                    }
                    tokio::time::sleep(self.config.backoff(retry - 1)).await;
                }
            }
        }

        let order = SwapOrder {
            settlement_tx_hash: Some(request.tx_hash),
            settlement_attempt: attempt,
            ..order
        };
        self.reconcile_order(&order).await
    }

    async fn fail(
        &self,
        order_id: Uuid,
        attempt: i32,
        reason_code: &str,
        raw: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE swap_orders SET status = 'failed', failure_reason_code = $2,
                    settlement_external_status = $3, updated_at = NOW()
             WHERE id = $1 AND status = 'settlement_pending'",
        )
        .bind(order_id)
        .bind(reason_code)
        .bind(raw)
        .execute(&self.pool)
        .await?;
        self.event(
            order_id,
            "settlement_failed",
            Some(attempt),
            None,
            Some(raw),
            json!({ "reason_code": reason_code }),
        )
        .await
    }

    /// Check external status of a recorded reference; never re-submits.
    async fn reconcile_order(&self, order: &SwapOrder) -> Result<SettlementOutcome, sqlx::Error> {
        let Some(hash) = order.settlement_tx_hash.as_deref() else {
            return Ok(SettlementOutcome::Pending);
        };
        let attempt = order.settlement_attempt;
        match self.adapter.status(hash).await {
            Ok(ExternalStatus::Confirmed { raw }) => {
                let mut tx = self.pool.begin().await?;
                let updated = sqlx::query(
                    "UPDATE swap_orders SET status = 'settled', settled_at = NOW(),
                            settlement_external_status = $2, updated_at = NOW()
                     WHERE id = $1 AND status = 'settlement_pending'",
                )
                .bind(order.id)
                .bind(&raw)
                .execute(&mut *tx)
                .await?
                .rows_affected();
                if updated > 0 {
                    sqlx::query(
                        "INSERT INTO swap_fills (order_id, settlement_tx_hash) VALUES ($1, $2)",
                    )
                    .bind(order.id)
                    .bind(hash)
                    .execute(&mut *tx)
                    .await?;
                }
                tx.commit().await?;
                if updated > 0 {
                    self.event(
                        order.id,
                        "settlement_confirmed",
                        Some(attempt),
                        Some(hash),
                        Some(&raw),
                        json!({}),
                    )
                    .await?;
                }
                Ok(SettlementOutcome::Settled)
            }
            Ok(ExternalStatus::Rejected { raw, reason_code })
            | Err(AdapterError::Rejected { reason_code, raw }) => {
                self.fail(order.id, attempt, &reason_code, &raw).await?;
                Ok(SettlementOutcome::Failed { reason_code })
            }
            Ok(ExternalStatus::Pending { raw }) => {
                sqlx::query("UPDATE swap_orders SET settlement_external_status = $2 WHERE id = $1")
                    .bind(order.id)
                    .bind(&raw)
                    .execute(&self.pool)
                    .await?;
                Ok(SettlementOutcome::Pending)
            }
            // Timeouts / 5xx: unknown, leave pending.
            Err(AdapterError::Unknown(_)) => Ok(SettlementOutcome::Pending),
        }
    }

    /// Re-check `SettlementPending` orders older than the threshold and alert
    /// on those that remain unresolved. Returns the number of orders examined.
    pub async fn reconcile_stuck(&self, alert: &dyn StuckAlert) -> Result<usize, sqlx::Error> {
        let orders: Vec<SwapOrder> = sqlx::query_as(&format!(
            "SELECT {ORDER_COLUMNS} FROM swap_orders
             WHERE status = 'settlement_pending'
               AND settlement_started_at < NOW() - make_interval(secs => $1)"
        ))
        .bind(self.config.stuck_threshold.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        for order in &orders {
            if self.reconcile_order(order).await? == SettlementOutcome::Pending {
                alert
                    .order_stuck(order.id, order.settlement_tx_hash.as_deref())
                    .await;
            }
        }
        Ok(orders.len())
    }
}

/// Background loop that periodically reconciles stuck settlements.
pub async fn run_reconciliation_loop<A: SettlementAdapter>(service: SettlementService<A>) {
    let mut ticker = tokio::time::interval(service.config.reconcile_interval);
    loop {
        ticker.tick().await;
        if let Err(e) = service.reconcile_stuck(&LogStuckAlert).await {
            tracing::error!("swap settlement reconciliation failed: {e}");
        }
    }
}
