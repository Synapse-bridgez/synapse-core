use failsafe::futures::CircuitBreaker as FuturesCircuitBreaker;
use failsafe::{backoff, failure_policy, Config, Error as FailsafeError, StateMachine};
use futures_util::stream::StreamExt;
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::instrument;

#[derive(Error, Debug)]
pub enum HorizonError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    /// A 404 from Horizon's `/accounts/{id}` endpoint. This is a normal,
    /// expected business outcome — e.g. a deposit destination that hasn't
    /// been funded on-chain yet — not an infrastructure failure. Callers
    /// that use this variant to decide whether a transaction should
    /// terminally fail must not treat it as equivalent to a permanent
    /// error; see `process_batch` in `services/processor.rs` for the
    /// bounded-retry handling this requires.
    #[error("Account not found: {0}")]
    AccountNotFound(String),
    #[error("Invalid response from Horizon: {0}")]
    InvalidResponse(String),
    #[error("Circuit breaker open: {0}")]
    CircuitBreakerOpen(String),
    /// Horizon accepted the submission but the transaction failed on-chain
    /// (or was rejected at submission time) with a `tx_*` result code.
    /// `code` is the raw Horizon result code (e.g. `tx_bad_seq`), and
    /// `detail` carries the human-readable `result_codes` payload when
    /// Horizon provided one.
    #[error("Transaction submission failed: {code}{}", .detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default())]
    TransactionFailed {
        code: String,
        detail: Option<String>,
    },
    /// The submitted transaction is already included in the ledger (or is
    /// already known to Horizon). This is a *success* outcome for settlement
    /// purposes: the caller must look up the transaction by hash rather than
    /// re-applying it.
    #[error("Transaction already included: {0}")]
    TransactionAlreadyIncluded(String),
    /// The submission request timed out or the connection dropped before
    /// Horizon answered. The transaction may still be included — callers
    /// must treat this as "unknown" and reconcile by hash lookup.
    #[error("Transaction submission outcome unknown: {0}")]
    SubmissionOutcomeUnknown(String),
    /// The transaction was not found by hash. This is a normal, expected
    /// outcome while a transaction is still propagating to Horizon.
    #[error("Transaction not found: {0}")]
    TransactionNotFound(String),
}

impl Clone for HorizonError {
    fn clone(&self) -> Self {
        match self {
            Self::RequestError(e) => Self::InvalidResponse(e.to_string()),
            Self::AccountNotFound(s) => Self::AccountNotFound(s.clone()),
            Self::InvalidResponse(s) => Self::InvalidResponse(s.clone()),
            Self::CircuitBreakerOpen(s) => Self::CircuitBreakerOpen(s.clone()),
            Self::TransactionFailed { code, detail } => Self::TransactionFailed {
                code: code.clone(),
                detail: detail.clone(),
            },
            Self::TransactionAlreadyIncluded(s) => Self::TransactionAlreadyIncluded(s.clone()),
            Self::SubmissionOutcomeUnknown(s) => Self::SubmissionOutcomeUnknown(s.clone()),
            Self::TransactionNotFound(s) => Self::TransactionNotFound(s.clone()),
        }
    }
}

/// Maps a Horizon client error onto a scorecard outcome.
pub(crate) fn classify_horizon_error(
    e: &HorizonError,
) -> crate::services::dependency_scorecard::CallOutcome {
    use crate::services::dependency_scorecard::{classify_reqwest_error, CallOutcome};
    match e {
        HorizonError::RequestError(re) => classify_reqwest_error(re),
        HorizonError::AccountNotFound(_) => CallOutcome::Success,
        // A 4xx ("Horizon API error: 400 Bad Request") is Horizon answering
        // a request it considers ours to fix; 5xx statuses and undecodable
        // bodies are Horizon answering badly.
        HorizonError::InvalidResponse(msg) => {
            if msg.starts_with("Horizon API error: 4") {
                CallOutcome::Success
            } else {
                CallOutcome::DependencyFault
            }
        }
        HorizonError::CircuitBreakerOpen(_) => CallOutcome::CircuitRejected,
        // A `tx_*` result code is the network answering normally about a
        // transaction it considers ours to fix; likewise a not-found lookup
        // and an already-included transaction are normal business outcomes.
        HorizonError::TransactionFailed { .. }
        | HorizonError::TransactionAlreadyIncluded(_)
        | HorizonError::TransactionNotFound(_) => CallOutcome::Success,
        // A submission timeout means we don't know whether Horizon is
        // healthy; treat it as a dependency fault so the scorecard reflects
        // the uncertainty rather than masking it.
        HorizonError::SubmissionOutcomeUnknown(_) => CallOutcome::DependencyFault,
    }
}

/// Response from Horizon /accounts endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: String,
    pub account_id: String,
    pub balances: Vec<Balance>,
    pub sequence: String,
    pub subentry_count: i32,
    pub home_domain: Option<String>,
    pub last_modified_ledger: i64,
    pub last_modified_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub balance: String,
    pub limit: Option<String>,
    pub asset_type: String,
    pub asset_code: Option<String>,
    pub asset_issuer: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamPayment {
    pub id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamMetrics {
    pub reconnections: u64,
    pub events_received: u64,
    pub last_event_time: Option<std::time::Instant>,
}

/// A single payment operation reported by Horizon's
/// `/accounts/{id}/payments` endpoint — used to verify that a *specific*
/// expected payment actually arrived, as opposed to `get_account`, which
/// only confirms the destination account currently exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaymentRecord {
    id: String,
    from: String,
    to: String,
    amount: String,
    asset_code: String,
    #[serde(default)]
    memo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PaymentsResponse {
    #[serde(rename = "_embedded")]
    embedded: PaymentsEmbedded,
}

#[derive(Debug, Deserialize)]
struct PaymentsEmbedded {
    records: Vec<PaymentRecord>,
}

/// Response body from Horizon's `POST /transactions` endpoint. On success
/// Horizon returns the full transaction record; on failure it returns a
/// `result_codes` object describing why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitTransactionResponse {
    pub hash: String,
    pub ledger: Option<i64>,
    pub successful: Option<bool>,
    #[serde(default)]
    pub result_codes: Option<TransactionResultCodes>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionResultCodes {
    #[serde(default)]
    pub transaction: Option<String>,
    #[serde(default)]
    pub operations: Option<Vec<String>>,
}

/// A transaction record as returned by `GET /transactions/{hash}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub hash: String,
    pub ledger: i64,
    pub successful: bool,
    pub source_account: String,
    #[serde(default)]
    pub source_account_sequence: Option<String>,
    #[serde(default)]
    pub result_codes: Option<TransactionResultCodes>,
}

#[derive(Debug, Deserialize)]
struct HorizonErrorBody {
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    extras: Option<HorizonErrorExtras>,
}

#[derive(Debug, Deserialize)]
struct HorizonErrorExtras {
    #[serde(default)]
    result_codes: Option<TransactionResultCodes>,
}

/// Classifies a Horizon submission failure into a typed `HorizonError`.
///
/// Horizon reports submission failures in two shapes:
/// * `400 Bad Request` with `extras.result_codes.transaction` set to a
///   `tx_*` code (e.g. `tx_bad_seq`, `tx_too_low`, `tx_too_late`).
/// * `409 Conflict` / `400` with a `detail` mentioning the transaction is
///   already included or already known.
///
/// This is exposed for unit testing so the retry classifier's inputs can be
/// verified without a live Horizon.
pub(crate) fn classify_submission_failure(
    status: u16,
    body: Option<&str>,
) -> HorizonError {
    let parsed: Option<HorizonErrorBody> =
        body.and_then(|b| serde_json::from_str::<HorizonErrorBody>(b).ok());

    let tx_code = parsed
        .as_ref()
        .and_then(|p| p.extras.as_ref())
        .and_then(|e| e.result_codes.as_ref())
        .and_then(|rc| rc.transaction.clone());

    if let Some(code) = tx_code {
        // `tx_bad_seq` and `tx_too_low` are distinct codes but both mean
        // "the sequence number we used is wrong" — keep them distinct so
        // the retry classifier can decide whether to re-allocate.
        return HorizonError::TransactionFailed {
            code,
            detail: parsed
                .as_ref()
                .and_then(|p| p.detail.clone())
                .or_else(|| parsed.as_ref().and_then(|p| p.title.clone())),
        };
    }

    let detail = parsed
        .as_ref()
        .and_then(|p| p.detail.clone())
        .or_else(|| parsed.as_ref().and_then(|p| p.title.clone()))
        .or_else(|| body.map(|b| b.to_string()));

    let detail_lower = detail.as_deref().unwrap_or("").to_lowercase();
    if status == 409
        || detail_lower.contains("already")
        || detail_lower.contains("duplicate")
    {
        return HorizonError::TransactionAlreadyIncluded(
            detail.unwrap_or_else(|| "transaction already included".to_string()),
        );
    }

    HorizonError::InvalidResponse(format!(
        "Horizon API error: {} {}",
        status,
        detail.unwrap_or_default()
    ))
}

/// HTTP client for interacting with the Stellar Horizon API
#[derive(Clone)]
pub struct HorizonClient {
    pub(crate) client: Client,
    pub(crate) base_url: String,
    circuit_breaker: StateMachine<failure_policy::ConsecutiveFailures<backoff::EqualJittered>, ()>,
}

impl HorizonClient {
    /// Creates a new HorizonClient with the specified base URL and circuit breaker
    pub fn new(base_url: String) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let backoff = backoff::equal_jittered(Duration::from_secs(60), Duration::from_secs(120));
        let policy = failure_policy::consecutive_failures(3, backoff);
        let circuit_breaker = Config::new().failure_policy(policy).build();

        HorizonClient {
            client,
            base_url,
            circuit_breaker,
        }
    }

    /// Creates a new HorizonClient with custom circuit breaker configuration
    pub fn with_circuit_breaker(
        base_url: String,
        failure_threshold: u32,
        reset_timeout_secs: u64,
    ) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let backoff = backoff::equal_jittered(
            Duration::from_secs(reset_timeout_secs),
            Duration::from_secs(reset_timeout_secs * 2),
        );
        let policy = failure_policy::consecutive_failures(failure_threshold, backoff);
        let circuit_breaker = Config::new().failure_policy(policy).build();

        HorizonClient {
            client,
            base_url,
            circuit_breaker,
        }
    }

    /// Feeds one settlement API call's outcome and any circuit-breaker state
    /// change into the dependency scorecard (#1334). A 404 is the network
    /// answering normally (an unfunded account), so it counts as available
    /// here even though the breaker's failure policy counts it.
    fn record_scorecard<T>(
        &self,
        permitted_before: bool,
        started: std::time::Instant,
        result: &Result<T, FailsafeError<HorizonError>>,
    ) {
        use crate::services::dependency_scorecard::{self as scorecard, CallOutcome, Dependency};
        let outcome = match result {
            Ok(_) => CallOutcome::Success,
            Err(FailsafeError::Rejected) => CallOutcome::CircuitRejected,
            Err(FailsafeError::Inner(e)) => classify_horizon_error(e),
        };
        scorecard::record_call(Dependency::SettlementApi, outcome, started.elapsed());
        scorecard::record_permitted_change(
            Dependency::SettlementApi,
            permitted_before,
            self.circuit_breaker.is_call_permitted(),
        );
    }

    /// Returns the current state of the circuit breaker
    pub fn circuit_state(&self) -> String {
        if self.circuit_breaker.is_call_permitted() {
            "closed".to_string()
        } else {
            "open".to_string()
        }
    }

    /// Fetches account details from the Horizon API.
    /// The current trace context is propagated via W3C `traceparent` headers.
    #[instrument(name = "horizon.get_account", skip(self), fields(stellar.account = %address))]
    pub async fn get_account(&self, address: &str) -> Result<AccountResponse, HorizonError> {
        let url = format!(
            "{}/accounts/{}",
            self.base_url.trim_end_matches('/'),
            address
        );
        let client = self.client.clone();
        let addr = address.to_string();

        // Inject W3C traceparent / tracestate into outgoing request headers.
        let mut headers = std::collections::HashMap::new();
        let propagator = TraceContextPropagator::new();
        let cx = opentelemetry::Context::current();
        propagator.inject_context(&cx, &mut headers);

        let permitted_before = self.circuit_breaker.is_call_permitted();
        let started = std::time::Instant::now();
        let result = self
            .circuit_breaker
            .call(async move {
                let mut req = client.get(&url);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                let response = req.send().await?;

                if !response.status().is_success() {
                    if response.status() == 404 {
                        return Err(HorizonError::AccountNotFound(addr));
                    }
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }

                let account = response.json::<AccountResponse>().await?;
                Ok(account)
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(account) => Ok(account),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Fetches the most recent payments to/from `address` from Horizon's
    /// `/accounts/{address}/payments` endpoint (newest first). Unlike
    /// `get_account`, this reports actual payment operations — the only way
    /// to confirm a *specific* expected payment (amount, asset, memo) was
    /// received, rather than just that the account exists on-chain.
    #[instrument(name = "horizon.list_payments", skip(self), fields(stellar.account = %address))]
    pub async fn list_payments_for_account(
        &self,
        address: &str,
        limit: u32,
    ) -> Result<Vec<Payment>, HorizonError> {
        let url = format!(
            "{}/accounts/{}/payments?order=desc&limit={}",
            self.base_url.trim_end_matches('/'),
            address,
            limit
        );
        let client = self.client.clone();
        let addr = address.to_string();

        let mut headers = std::collections::HashMap::new();
        let propagator = TraceContextPropagator::new();
        let cx = opentelemetry::Context::current();
        propagator.inject_context(&cx, &mut headers);

        let permitted_before = self.circuit_breaker.is_call_permitted();
        let started = std::time::Instant::now();
        let result = self
            .circuit_breaker
            .call(async move {
                let mut req = client.get(&url);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                let response = req.send().await?;

                if !response.status().is_success() {
                    if response.status() == 404 {
                        return Err(HorizonError::AccountNotFound(addr));
                    }
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }

                let parsed = response.json::<PaymentsResponse>().await?;
                Ok(parsed
                    .embedded
                    .records
                    .into_iter()
                    .map(|r| Payment {
                        id: r.id,
                        from: r.from,
                        to: r.to,
                        amount: r.amount,
                        asset_code: r.asset_code,
                        memo: r.memo,
                    })
                    .collect::<Vec<_>>())
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(payments) => Ok(payments),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Stream payments for an account via SSE with automatic reconnection.
    ///
    /// # Reconnect contract
    ///
    /// This function never returns on its own once started: both a clean
    /// stream close (`connect_stream` returning `Ok`) *and* any failure —
    /// initial connect error, a non-2xx response, or a transport error
    /// mid-stream (`connect_stream` returning `Err`) — are reconnected with
    /// the same exponential backoff. A caller that only reconnected on the
    /// `Ok` case would silently stop receiving payments on the very first
    /// transient network blip or Horizon 5xx, with no on-chain payment
    /// visible again until the process was restarted — do not reintroduce
    /// an early `return Err(..)` here.
    #[instrument(name = "horizon.stream_payments", skip(self), fields(stellar.account = %account))]
    pub async fn stream_payments(
        &self,
        account: &str,
        tx: mpsc::Sender<Result<StreamPayment, HorizonError>>,
    ) -> Result<(), HorizonError> {
        let mut reconnect_count = 0u64;
        let metrics = Arc::new(tokio::sync::Mutex::new(StreamMetrics {
            reconnections: 0,
            events_received: 0,
            last_event_time: None,
        }));

        loop {
            let url = format!(
                "{}/accounts/{}/payments?order=asc&stream=true",
                self.base_url.trim_end_matches('/'),
                account
            );

            let outcome = self.connect_stream(&url, &tx, &metrics).await;

            reconnect_count += 1;
            {
                let mut m = metrics.lock().await;
                m.reconnections = reconnect_count;
            }

            let reason = match &outcome {
                Ok(_) => "clean_close",
                Err(_) => "error",
            };
            crate::metrics::stream_reconnect_total()
                .add(1, &[opentelemetry::KeyValue::new("reason", reason)]);

            if let Err(e) = &outcome {
                tracing::warn!(
                    "Stream error for {}, reconnecting (attempt {}): {}",
                    account,
                    reconnect_count,
                    e
                );
                // Surface the error to the caller (e.g. for logging/metrics
                // on their side) without terminating the stream — the whole
                // point of this fix is that a transport failure is not
                // grounds to stop reconnecting.
                let _ = tx.send(Err(e.clone())).await;
            } else {
                tracing::warn!(
                    "Stream disconnected for {}, reconnecting (attempt {})",
                    account,
                    reconnect_count
                );
            }

            // Exponential backoff: 2s, 4s, 8s, ..., capped at 30s. The shift
            // exponent is clamped so `reconnect_count` growing unbounded
            // over a long-lived, frequently-erroring stream can never
            // overflow the shift.
            let backoff_secs = std::cmp::min(1u64 << reconnect_count.min(6), 30);
            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
        }
    }

    async fn connect_stream(
        &self,
        url: &str,
        tx: &mpsc::Sender<Result<StreamPayment, HorizonError>>,
        metrics: &Arc<tokio::sync::Mutex<StreamMetrics>>,
    ) -> Result<(), HorizonError> {
        let response = self
            .client
            .get(url)
            .header("Accept", "text/event-stream")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(HorizonError::InvalidResponse(format!(
                "Stream connection failed: {}",
                response.status()
            )));
        }

        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk: bytes::Bytes = chunk?;
            let text = String::from_utf8_lossy(&chunk);

            for line in text.lines() {
                if let Some(json_str) = line.strip_prefix("data: ") {
                    match serde_json::from_str::<StreamPayment>(json_str) {
                        Ok(payment) => {
                            let mut m = metrics.lock().await;
                            m.events_received += 1;
                            m.last_event_time = Some(std::time::Instant::now());
                            drop(m);

                            if tx.send(Ok(payment)).await.is_err() {
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse payment event: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn get_stream_metrics(
        &self,
        metrics: &Arc<tokio::sync::Mutex<StreamMetrics>>,
    ) -> StreamMetrics {
        *metrics.lock().await
    }

    /// Submits a base64-encoded signed transaction envelope to Horizon's
    /// `POST /transactions` endpoint.
    ///
    /// # Outcome contract
    ///
    /// * `Ok(SubmitTransactionResponse)` — Horizon accepted the transaction
    ///   (it may still be pending; check `successful`/`ledger`).
    /// * `Err(HorizonError::TransactionFailed { code, .. })` — Horizon
    ///   rejected the transaction with a `tx_*` result code. The retry
    ///   classifier decides whether this is transient (`tx_bad_seq`,
    ///   `tx_too_low`, `tx_too_late`) or terminal.
    /// * `Err(HorizonError::TransactionAlreadyIncluded(_))` — the
    ///   transaction is already known to Horizon; callers must reconcile by
    ///   hash lookup rather than re-applying.
    /// * `Err(HorizonError::SubmissionOutcomeUnknown(_))` — the request
    ///   timed out or the connection dropped. The transaction may still be
    ///   included; callers must reconcile by hash lookup.
    ///
    /// The signed envelope is never logged: it is passed straight into the
    /// request body and only the transaction hash is surfaced in traces.
    #[instrument(name = "horizon.submit_transaction", skip(self, envelope_xdr), fields(stellar.tx_hash = tracing::field::Empty))]
    pub async fn submit_transaction(
        &self,
        envelope_xdr: &str,
    ) -> Result<SubmitTransactionResponse, HorizonError> {
        let url = format!("{}/transactions", self.base_url.trim_end_matches('/'));
        let client = self.client.clone();
        let envelope = envelope_xdr.to_string();

        let mut headers = std::collections::HashMap::new();
        let propagator = TraceContextPropagator::new();
        let cx = opentelemetry::Context::current();
        propagator.inject_context(&cx, &mut headers);

        let permitted_before = self.circuit_breaker.is_call_permitted();
        let started = std::time::Instant::now();
        let result = self
            .circuit_breaker
            .call(async move {
                let mut req = client
                    .post(&url)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .form(&[("tx", envelope.as_str())]);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                let response = match req.send().await {
                    Ok(r) => r,
                    Err(e) => {
                        // A timeout or transport error means we don't know
                        // whether Horizon included the transaction.
                        if e.is_timeout() || e.is_connect() || e.is_request() {
                            return Err(HorizonError::SubmissionOutcomeUnknown(e.to_string()));
                        }
                        return Err(HorizonError::RequestError(e));
                    }
                };

                let status = response.status();
                let body = response.text().await.unwrap_or_default();

                if status.is_success() {
                    let parsed = serde_json::from_str::<SubmitTransactionResponse>(&body)
                        .map_err(|e| HorizonError::InvalidResponse(e.to_string()))?;
                    return Ok(parsed);
                }

                Err(classify_submission_failure(status.as_u16(), Some(&body)))
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(resp) => Ok(resp),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Fetches a transaction record by hash from `GET /transactions/{hash}`.
    ///
    /// Returns `Err(HorizonError::TransactionNotFound(_))` on a 404, which
    /// is the normal outcome while a transaction is still propagating. This
    /// is the idempotent read of truth used to detect duplicate submissions
    /// and to reconcile submissions whose outcome was unknown.
    #[instrument(name = "horizon.get_transaction", skip(self), fields(stellar.tx_hash = %tx_hash))]
    pub async fn get_transaction(
        &self,
        tx_hash: &str,
    ) -> Result<TransactionRecord, HorizonError> {
        let url = format!(
            "{}/transactions/{}",
            self.base_url.trim_end_matches('/'),
            tx_hash
        );
        let client = self.client.clone();
        let hash = tx_hash.to_string();

        let mut headers = std::collections::HashMap::new();
        let propagator = TraceContextPropagator::new();
        let cx = opentelemetry::Context::current();
        propagator.inject_context(&cx, &mut headers);

        let permitted_before = self.circuit_breaker.is_call_permitted();
        let started = std::time::Instant::now();
        let result = self
            .circuit_breaker
            .call(async move {
                let mut req = client.get(&url);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                let response = req.send().await?;

                if response.status() == 404 {
                    return Err(HorizonError::TransactionNotFound(hash));
                }
                if !response.status().is_success() {
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }

                let record = response.json::<TransactionRecord>().await?;
                Ok(record)
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(record) => Ok(record),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Reads the current ledger sequence from Horizon's `/ledgers?order=desc&limit=1`
    /// endpoint. Used to bound `tx_too_late` waits and to confirm that a
    /// transaction's inclusion ledger has closed.
    #[instrument(name = "horizon.latest_ledger", skip(self))]
    pub async fn latest_ledger_sequence(&self) -> Result<i64, HorizonError> {
        #[derive(Debug, Deserialize)]
        struct LedgersResponse {
            #[serde(rename = "_embedded")]
            embedded: LedgersEmbedded,
        }
        #[derive(Debug, Deserialize)]
        struct LedgersEmbedded {
            records: Vec<LedgerRecord>,
        }
        #[derive(Debug, Deserialize)]
        struct LedgerRecord {
            sequence: i64,
        }

        let url = format!(
            "{}/ledgers?order=desc&limit=1",
            self.base_url.trim_end_matches('/')
        );
        let client = self.client.clone();

        let permitted_before = self.circuit_breaker.is_call_permitted();
        let started = std::time::Instant::now();
        let result = self
            .circuit_breaker
            .call(async move {
                let response = client.get(&url).send().await?;
                if !response.status().is_success() {
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }
                let parsed = response.json::<LedgersResponse>().await?;
                parsed
                    .embedded
                    .records
                    .into_iter()
                    .next()
                    .map(|r| r.sequence)
                    .ok_or_else(|| {
                        HorizonError::InvalidResponse("no ledgers returned".to_string())
                    })
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(seq) => Ok(seq),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_horizon_client_creation() {
        let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        assert_eq!(client.base_url, "https://horizon-testnet.stellar.org");
    }

    #[tokio::test]
    async fn test_get_account_with_mock() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "account_id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "balances": [
                {
                    "balance": "100.0000000",
                    "asset_type": "native",
                    "limit": null,
                    "asset_code": null,
                    "asset_issuer": null
                }
            ],
            "sequence": "1",
            "subentry_count": 0,
            "home_domain": null,
            "last_modified_ledger": 1,
            "last_modified_time": "2021-01-01T00:00:00Z"
        }"#;

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let account = client
            .get_account("GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ")
            .await;

        assert!(account.is_ok());
        let acc = account.unwrap();
        assert_eq!(
            acc.account_id,
            "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_account_not_found() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*".into()))
            .with_status(404)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let result = client
            .get_account("GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ")
            .await;

        assert!(matches!(result, Err(HorizonError::AccountNotFound(_))));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_list_payments_for_account_with_mock() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "_embedded": {
                "records": [
                    {
                        "id": "12345",
                        "from": "GSENDER",
                        "to": "GRECEIVER",
                        "amount": "42.5000000",
                        "asset_code": "USD",
                        "memo": "invoice-1"
                    }
                ]
            }
        }"#;

        let mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let payments = client
            .list_payments_for_account("GRECEIVER", 50)
            .await
            .expect("expected payments list");

        assert_eq!(payments.len(), 1);
        assert_eq!(payments[0].to, "GRECEIVER");
        assert_eq!(payments[0].amount, "42.5000000");
        assert_eq!(payments[0].memo.as_deref(), Some("invoice-1"));
        mock.assert_async().await;
    }

    /// Part B regression test: prior to this fix, `connect_stream` returning
    /// an `Err` (initial connect failure, non-2xx response, or a mid-stream
    /// transport error) made `stream_payments` send the error once and
    /// return immediately — the reconnect-with-backoff loop only ever fired
    /// on a clean `Ok` close. Receiving a *second* error here proves the
    /// loop retried instead of terminating after the first one.
    #[tokio::test]
    async fn test_stream_reconnects_after_transport_error() {
        let mut server = mockito::Server::new_async().await;
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(503)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let (tx, mut rx) = mpsc::channel(16);

        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            let _ = client_clone.stream_payments("GTEST", tx).await;
        });

        let first = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("expected a first error before timing out")
            .expect("channel closed unexpectedly");
        assert!(matches!(first, Err(HorizonError::InvalidResponse(_))));

        let second = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("expected a second error after reconnect before timing out")
            .expect("channel closed unexpectedly");
        assert!(matches!(second, Err(HorizonError::InvalidResponse(_))));

        handle.abort();
    }

    /// A single malformed/unparseable SSE `data:` line must not stop the
    /// rest of the response from being processed — a later, well-formed
    /// event on the same connection should still reach the caller. This
    /// covers the parsing-loop resilience the issue's "oversized SSE event"
    /// scenario was pointing at; current code has no buffer-accumulation
    /// step for a specific buffer-overflow guard to reset (each chunk's
    /// lines are parsed independently), so this test exercises the
    /// equivalent "one bad event doesn't kill the stream" property instead.
    #[tokio::test]
    async fn test_malformed_sse_event_does_not_block_subsequent_events() {
        let mut server = mockito::Server::new_async().await;
        let body = "data: not-valid-json\ndata: {\"id\":\"1\",\"from\":\"GA\",\"to\":\"GB\",\"amount\":\"10\",\"asset_code\":\"XLM\",\"created_at\":\"2026-01-01T00:00:00Z\"}\n";
        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(200)
            .with_body(body)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let (tx, mut rx) = mpsc::channel(16);

        let client_clone = client.clone();
        let handle = tokio::spawn(async move {
            let _ = client_clone.stream_payments("GTEST", tx).await;
        });

        let received = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("expected the valid event after the malformed one before timing out")
            .expect("channel closed unexpectedly");
        let payment = received.expect("expected Ok(payment), got an error");
        assert_eq!(payment.id, "1");

        handle.abort();
    }

    #[tokio::test]
    async fn test_list_payments_for_account_not_found() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/accounts/.*/payments.*".into()),
            )
            .with_status(404)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let result = client.list_payments_for_account("GMISSING", 50).await;

        assert!(matches!(result, Err(HorizonError::AccountNotFound(_))));
        mock.assert_async().await;
    }

    #[test]
    fn test_circuit_breaker_state() {
        let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        let state = client.circuit_state();
        assert_eq!(state, "closed");
    }

    #[test]
    fn test_custom_circuit_breaker_config() {
        let client = HorizonClient::with_circuit_breaker(
            "https://horizon-testnet.stellar.org".to_string(),
            5,
            30,
        );
        let state = client.circuit_state();
        assert_eq!(state, "closed");
    }

    #[test]
    fn test_classify_submission_failure_tx_bad_seq() {
        let body = r#"{
            "status": 400,
            "title": "Transaction Failed",
            "detail": "The transaction failed when submitted to the stellar network.",
            "extras": {
                "result_codes": {
                    "transaction": "tx_bad_seq"
                }
            }
        }"#;
        let err = classify_submission_failure(400, Some(body));
        match err {
            HorizonError::TransactionFailed { code, .. } => assert_eq!(code, "tx_bad_seq"),
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_submission_failure_tx_too_low() {
        let body = r#"{
            "status": 400,
            "extras": { "result_codes": { "transaction": "tx_too_low" } }
        }"#;
        let err = classify_submission_failure(400, Some(body));
        match err {
            HorizonError::TransactionFailed { code, .. } => assert_eq!(code, "tx_too_low"),
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_submission_failure_tx_too_late() {
        let body = r#"{
            "status": 400,
            "extras": { "result_codes": { "transaction": "tx_too_late" } }
        }"#;
        let err = classify_submission_failure(400, Some(body));
        match err {
            HorizonError::TransactionFailed { code, .. } => assert_eq!(code, "tx_too_late"),
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_submission_failure_duplicate() {
        let body = r#"{
            "status": 409,
            "title": "Transaction Already Exists",
            "detail": "The transaction already exists in the ledger."
        }"#;
        let err = classify_submission_failure(409, Some(body));
        assert!(matches!(
            err,
            HorizonError::TransactionAlreadyIncluded(_)
        ));
    }

    #[test]
    fn test_classify_submission_failure_duplicate_by_detail() {
        let body = r#"{ "detail": "duplicate transaction" }"#;
        let err = classify_submission_failure(400, Some(body));
        assert!(matches!(
            err,
            HorizonError::TransactionAlreadyIncluded(_)
        ));
    }

    #[tokio::test]
    async fn test_submit_transaction_success() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "hash": "abc123",
            "ledger": 42,
            "successful": true
        }"#;
        let mock = server
            .mock("POST", "/transactions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let resp = client
            .submit_transaction("AAAA...")
            .await
            .expect("expected successful submission");
        assert_eq!(resp.hash, "abc123");
        assert_eq!(resp.ledger, Some(42));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_submit_transaction_bad_seq() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "status": 400,
            "extras": { "result_codes": { "transaction": "tx_bad_seq" } }
        }"#;
        let mock = server
            .mock("POST", "/transactions")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let err = client
            .submit_transaction("AAAA...")
            .await
            .expect_err("expected tx_bad_seq");
        match err {
            HorizonError::TransactionFailed { code, .. } => assert_eq!(code, "tx_bad_seq"),
            other => panic!("expected TransactionFailed, got {other:?}"),
        }
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_transaction_not_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/transactions/.*".into()))
            .with_status(404)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let err = client
            .get_transaction("deadbeef")
            .await
            .expect_err("expected not found");
        assert!(matches!(err, HorizonError::TransactionNotFound(_)));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_transaction_found() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "hash": "deadbeef",
            "ledger": 100,
            "successful": true,
            "source_account": "GSRC",
            "source_account_sequence": "12345"
        }"#;
        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/transactions/.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let record = client
            .get_transaction("deadbeef")
            .await
            .expect("expected record");
        assert_eq!(record.hash, "deadbeef");
        assert_eq!(record.ledger, 100);
        assert!(record.successful);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_latest_ledger_sequence() {
        let mut server = mockito::Server::new_async().await;
        let body = r#"{
            "_embedded": {
                "records": [ { "sequence": 777 } ]
            }
        }"#;
        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/ledgers.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let seq = client
            .latest_ledger_sequence()
            .await
            .expect("expected ledger sequence");
        assert_eq!(seq, 777);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_after_failures() {
        let mut server = mockito::Server::new_async().await;

        let mock = server
            .mock("GET", mockito::Matcher::Regex(r"^/accounts/.*".into()))
            .with_status(500)
            .expect_at_least(3)
            .create_async()
            .await;

        let client = HorizonClient::with_circuit_breaker(server.url(), 3, 60);

        // Make 3 failing requests to trip the circuit breaker
        for _ in 0..3 {
            let _ = client.get_account("TEST_ACCOUNT").await;
        }

        // The next request should be rejected by the open circuit breaker
        let result = client.get_account("TEST_ACCOUNT").await;
        assert!(
            matches!(result, Err(HorizonError::CircuitBreakerOpen(_))),
            "Expected CircuitBreakerOpen, got: {:?}",
            result
        );
        mock.assert_async().await;
    }
}
