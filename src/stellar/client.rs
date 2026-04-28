use failsafe::futures::CircuitBreaker as FuturesCircuitBreaker;
use failsafe::{backoff, failure_policy, Config, Error as FailsafeError, StateMachine};
use opentelemetry::propagation::TextMapPropagator;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tracing::instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use futures_util::stream::StreamExt;
use tokio::sync::mpsc;

#[derive(Error, Debug)]
pub enum HorizonError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("Account not found: {0}")]
    AccountNotFound(String),
    #[error("Invalid response from Horizon: {0}")]
    InvalidResponse(String),
    #[error("Circuit breaker open: {0}")]
    CircuitBreakerOpen(String),
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
        let cx = tracing::Span::current().context();
        propagator.inject_context(&cx, &mut headers);

        let result = self
            .circuit_breaker
            .call(async move {
                let mut req = client.get(&url);
                for (k, v) in &headers {
                    req = req.header(k.as_str(), v.as_str());
                }
                let response = req.send().await?;

                if response.status() == 404 {
                    return Err(HorizonError::AccountNotFound(addr));
                }

                let account = response.json::<AccountResponse>().await?;
                Ok(account)
            })
            .await;

        match result {
            Ok(account) => Ok(account),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Stream payments for an account via SSE with automatic reconnection
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

            match self.connect_stream(&url, &tx, &metrics).await {
                Ok(_) => {
                    // Stream ended normally
                    reconnect_count += 1;
                    let mut m = metrics.lock().await;
                    m.reconnections = reconnect_count;
                    drop(m);

                    tracing::warn!(
                        "Stream disconnected for {}, reconnecting (attempt {})",
                        account,
                        reconnect_count
                    );

                    // Exponential backoff: 1s, 2s, 4s, 8s, max 30s
                    let backoff_secs = std::cmp::min(1u64 << reconnect_count, 30);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e.clone())).await;
                    return Err(e);
                }
            }
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
            let chunk = chunk?;
            let text = String::from_utf8_lossy(&chunk);

            for line in text.lines() {
                if line.starts_with("data: ") {
                    let json_str = &line[6..];
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
        metrics.lock().await.clone()
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
    #[ignore]
    async fn test_get_account_with_mock() {
        let mut server = mockito::Server::new_async().await;

        let mock_response = r#"{
            "id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "account_id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "balances": [
                {
                    "balance": "100.0000000",
                    "asset_type": "native",
                    "balance": "100.0000000",
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

        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r".*/accounts/.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create();

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
    }

    #[tokio::test]
    #[ignore]
    async fn test_get_account_not_found() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r".*/accounts/.*".into()))
            .with_status(404)
            .create();

        let client = HorizonClient::new(server.url());
        let result = client
            .get_account("GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ")
            .await;

        assert!(matches!(result, Err(HorizonError::AccountNotFound(_))));
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
    #[ignore]
    async fn test_circuit_breaker_opens_after_failures() {
        let mut server = mockito::Server::new_async().await;

        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r".*/accounts/.*".into()))
            .with_status(500)
            .expect_at_least(3)
            .create();

        let client = HorizonClient::with_circuit_breaker(server.url(), 3, 1);

        // Make 3 failing requests to trip the circuit breaker
        for _ in 0..3 {
            let _ = client.get_account("TEST_ACCOUNT").await;
        }

        // The next request should be rejected by the circuit breaker
        let result = client.get_account("TEST_ACCOUNT").await;
        assert!(matches!(result, Err(HorizonError::CircuitBreakerOpen(_))));
    }
}
