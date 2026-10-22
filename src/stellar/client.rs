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
}

impl Clone for HorizonError {
    fn clone(&self) -> Self {
        match self {
            Self::RequestError(e) => Self::InvalidResponse(e.to_string()),
            Self::AccountNotFound(s) => Self::AccountNotFound(s.clone()),
            Self::InvalidResponse(s) => Self::InvalidResponse(s.clone()),
            Self::CircuitBreakerOpen(s) => Self::CircuitBreakerOpen(s.clone()),
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

/// Raw response from Horizon's `/fee_stats` endpoint. All fee fields are
/// decimal strings in stroops; they are parsed into `i64` by
/// `HorizonClient::get_fee_stats` so callers never deal with string amounts.
#[derive(Debug, Clone, Deserialize)]
struct FeeStatsResponse {
    #[serde(default)]
    last_ledger: Option<String>,
    #[serde(default)]
    last_ledger_base_fee: Option<String>,
    #[serde(default)]
    ledger_capacity_usage: Option<String>,
    #[serde(default)]
    fee_charged: Option<FeePercentiles>,
    #[serde(default)]
    max_fee: Option<FeePercentiles>,
}

#[derive(Debug, Clone, Deserialize)]
struct FeePercentiles {
    #[serde(default)]
    min: Option<String>,
    #[serde(default)]
    max: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    p10: Option<String>,
    #[serde(default)]
    p50: Option<String>,
    #[serde(default)]
    p90: Option<String>,
    #[serde(default)]
    p95: Option<String>,
    #[serde(default)]
    p99: Option<String>,
}

/// Network fee statistics from Horizon, with every amount already parsed
/// into `i64` stroops. Percentile fields are `None` when Horizon omitted
/// them (e.g. an empty ledger), letting the estimator fall back to the
/// base fee.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeeStats {
    pub last_ledger: Option<i64>,
    pub last_ledger_base_fee: Option<i64>,
    /// Fraction (0.0..=1.0) of the last ledger's capacity that was used.
    pub ledger_capacity_usage: Option<f64>,
    /// Percentiles of fees actually charged, in stroops.
    pub fee_charged_min: Option<i64>,
    pub fee_charged_max: Option<i64>,
    pub fee_charged_mode: Option<i64>,
    pub fee_charged_p10: Option<i64>,
    pub fee_charged_p50: Option<i64>,
    pub fee_charged_p90: Option<i64>,
    pub fee_charged_p95: Option<i64>,
    pub fee_charged_p99: Option<i64>,
    /// Percentiles of the maximum fee bids seen, in stroops.
    pub max_fee_min: Option<i64>,
    pub max_fee_max: Option<i64>,
    pub max_fee_mode: Option<i64>,
    pub max_fee_p10: Option<i64>,
    pub max_fee_p50: Option<i64>,
    pub max_fee_p90: Option<i64>,
    pub max_fee_p95: Option<i64>,
    pub max_fee_p99: Option<i64>,
}

impl FeeStats {
    /// Returns the `fee_charged` percentile for the given integer percentile
    /// (10, 50, 90, 95 or 99). Unknown percentiles yield `None`.
    pub fn fee_charged_percentile(&self, percentile: u8) -> Option<i64> {
        match percentile {
            10 => self.fee_charged_p10,
            50 => self.fee_charged_p50,
            90 => self.fee_charged_p90,
            95 => self.fee_charged_p95,
            99 => self.fee_charged_p99,
            _ => None,
        }
    }

    /// Returns the `max_fee` percentile for the given integer percentile
    /// (10, 50, 90, 95 or 99). Unknown percentiles yield `None`.
    pub fn max_fee_percentile(&self, percentile: u8) -> Option<i64> {
        match percentile {
            10 => self.max_fee_p10,
            50 => self.max_fee_p50,
            90 => self.max_fee_p90,
            95 => self.max_fee_p95,
            99 => self.max_fee_p99,
            _ => None,
        }
    }
}

/// Parses a Horizon decimal-string amount into `i64` stroops, rejecting
/// values that are non-numeric or would overflow `i64`.
fn parse_stroops(field: &str, value: &Option<String>) -> Result<Option<i64>, HorizonError> {
    match value {
        None => Ok(None),
        Some(raw) => raw
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|e| HorizonError::InvalidResponse(format!("invalid {field} '{raw}': {e}"))),
    }
}

/// Outcome of looking up a transaction by hash on Horizon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionInclusion {
    /// Horizon has no record of the hash: the transaction was never
    /// submitted or has already been pruned from history.
    NotFound,
    /// Horizon knows the hash but it is not yet in a ledger.
    Pending,
    /// The transaction is included in a ledger.
    Included {
        ledger: i64,
        successful: bool,
        /// Fee actually charged, in stroops.
        fee_charged: i64,
    },
}

#[derive(Debug, Deserialize)]
struct TransactionRecord {
    #[serde(default)]
    ledger: Option<i64>,
    #[serde(default)]
    successful: Option<bool>,
    #[serde(default)]
    fee_charged: Option<String>,
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

    /// Fetches current network fee statistics from Horizon's `/fee_stats`
    /// endpoint. All fee amounts are returned as `i64` stroops.
    #[instrument(name = "horizon.get_fee_stats", skip(self))]
    pub async fn get_fee_stats(&self) -> Result<FeeStats, HorizonError> {
        let url = format!("{}/fee_stats", self.base_url.trim_end_matches('/'));
        let client = self.client.clone();

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
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }

                let raw = response.json::<FeeStatsResponse>().await?;
                let charged = raw.fee_charged.unwrap_or(FeePercentiles {
                    min: None,
                    max: None,
                    mode: None,
                    p10: None,
                    p50: None,
                    p90: None,
                    p95: None,
                    p99: None,
                });
                let max_fee = raw.max_fee.unwrap_or(FeePercentiles {
                    min: None,
                    max: None,
                    mode: None,
                    p10: None,
                    p50: None,
                    p90: None,
                    p95: None,
                    p99: None,
                });

                let ledger_capacity_usage = match raw.ledger_capacity_usage.as_deref() {
                    None => None,
                    Some(s) => Some(s.trim().parse::<f64>().map_err(|e| {
                        HorizonError::InvalidResponse(format!(
                            "invalid ledger_capacity_usage '{s}': {e}"
                        ))
                    })?),
                };

                Ok(FeeStats {
                    last_ledger: parse_stroops("last_ledger", &raw.last_ledger)?,
                    last_ledger_base_fee: parse_stroops(
                        "last_ledger_base_fee",
                        &raw.last_ledger_base_fee,
                    )?,
                    ledger_capacity_usage,
                    fee_charged_min: parse_stroops("fee_charged.min", &charged.min)?,
                    fee_charged_max: parse_stroops("fee_charged.max", &charged.max)?,
                    fee_charged_mode: parse_stroops("fee_charged.mode", &charged.mode)?,
                    fee_charged_p10: parse_stroops("fee_charged.p10", &charged.p10)?,
                    fee_charged_p50: parse_stroops("fee_charged.p50", &charged.p50)?,
                    fee_charged_p90: parse_stroops("fee_charged.p90", &charged.p90)?,
                    fee_charged_p95: parse_stroops("fee_charged.p95", &charged.p95)?,
                    fee_charged_p99: parse_stroops("fee_charged.p99", &charged.p99)?,
                    max_fee_min: parse_stroops("max_fee.min", &max_fee.min)?,
                    max_fee_max: parse_stroops("max_fee.max", &max_fee.max)?,
                    max_fee_mode: parse_stroops("max_fee.mode", &max_fee.mode)?,
                    max_fee_p10: parse_stroops("max_fee.p10", &max_fee.p10)?,
                    max_fee_p50: parse_stroops("max_fee.p50", &max_fee.p50)?,
                    max_fee_p90: parse_stroops("max_fee.p90", &max_fee.p90)?,
                    max_fee_p95: parse_stroops("max_fee.p95", &max_fee.p95)?,
                    max_fee_p99: parse_stroops("max_fee.p99", &max_fee.p99)?,
                })
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(stats) => Ok(stats),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Looks up a transaction by hash on Horizon's `/transactions/{hash}`
    /// endpoint to determine whether it has been included in a ledger.
    ///
    /// A 404 maps to `TransactionInclusion::NotFound` (a normal outcome for
    /// a not-yet-visible hash), not an error, so the bump policy can treat
    /// it as "still pending" without special-casing `HorizonError`.
    #[instrument(name = "horizon.get_transaction", skip(self), fields(stellar.tx_hash = %tx_hash))]
    pub async fn get_transaction_inclusion(
        &self,
        tx_hash: &str,
    ) -> Result<TransactionInclusion, HorizonError> {
        let url = format!(
            "{}/transactions/{}",
            self.base_url.trim_end_matches('/'),
            tx_hash
        );
        let client = self.client.clone();

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
                    return Ok(TransactionInclusion::NotFound);
                }
                if !response.status().is_success() {
                    return Err(HorizonError::InvalidResponse(format!(
                        "Horizon API error: {}",
                        response.status()
                    )));
                }

                let record = response.json::<TransactionRecord>().await?;
                match record.ledger {
                    None => Ok(TransactionInclusion::Pending),
                    Some(ledger) => {
                        let fee_charged =
                            parse_stroops("fee_charged", &record.fee_charged)?.unwrap_or(0);
                        Ok(TransactionInclusion::Included {
                            ledger,
                            successful: record.successful.unwrap_or(false),
                            fee_charged,
                        })
                    }
                }
            })
            .await;

        self.record_scorecard(permitted_before, started, &result);
        match result {
            Ok(inclusion) => Ok(inclusion),
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

    #[tokio::test]
    async fn test_get_fee_stats_with_mock() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "last_ledger": "12345",
            "last_ledger_base_fee": "100",
            "ledger_capacity_usage": "0.75",
            "fee_charged": {
                "min": "100", "max": "5000", "mode": "100",
                "p10": "100", "p50": "150", "p90": "800", "p95": "1200", "p99": "4000"
            },
            "max_fee": {
                "min": "100", "max": "9000", "mode": "100",
                "p10": "100", "p50": "200", "p90": "1500", "p95": "2500", "p99": "8000"
            }
        }"#;

        let mock = server
            .mock("GET", "/fee_stats")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let stats = client.get_fee_stats().await.expect("expected fee stats");

        assert_eq!(stats.last_ledger, Some(12345));
        assert_eq!(stats.last_ledger_base_fee, Some(100));
        assert_eq!(stats.ledger_capacity_usage, Some(0.75));
        assert_eq!(stats.fee_charged_p50, Some(150));
        assert_eq!(stats.fee_charged_percentile(90), Some(800));
        assert_eq!(stats.max_fee_percentile(99), Some(8000));
        assert_eq!(stats.fee_charged_percentile(42), None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_fee_stats_rejects_overflowing_amount() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "fee_charged": { "p50": "99999999999999999999999999" }
        }"#;

        let _mock = server
            .mock("GET", "/fee_stats")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let result = client.get_fee_stats().await;
        assert!(matches!(result, Err(HorizonError::InvalidResponse(_))));
    }

    #[tokio::test]
    async fn test_get_transaction_inclusion_included() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "hash": "abc",
            "ledger": 42,
            "successful": true,
            "fee_charged": "250"
        }"#;

        let mock = server
            .mock("GET", "/transactions/abc")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let inclusion = client
            .get_transaction_inclusion("abc")
            .await
            .expect("expected inclusion lookup");

        assert_eq!(
            inclusion,
            TransactionInclusion::Included {
                ledger: 42,
                successful: true,
                fee_charged: 250,
            }
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_transaction_inclusion_not_found() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("GET", "/transactions/missing")
            .with_status(404)
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let inclusion = client
            .get_transaction_inclusion("missing")
            .await
            .expect("404 should map to NotFound, not an error");

        assert_eq!(inclusion, TransactionInclusion::NotFound);
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
