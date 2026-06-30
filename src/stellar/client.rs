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
        let cx = opentelemetry::Context::current();
        propagator.inject_context(&cx, &mut headers);

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

        match result {
            Ok(account) => Ok(account),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen(
                "Horizon API circuit breaker is open".to_string(),
            )),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }

    /// Stream payments for an account via SSE with automatic reconnection.
    ///
    /// Resumes from `initial_cursor` (the Horizon paging token of the last
    /// successfully processed payment) on first connect and on every reconnect,
    /// so no on-chain payment in a disconnect window is missed or replayed.
    /// Pass `None` to start from the current live position ("now").
    ///
    /// Backoff is saturating: the shift exponent is capped at 5 (max 32 s,
    /// clamped to 30 s) so the counter can never overflow regardless of how
    /// many reconnects occur. The counter resets after any session that delivers
    /// at least one event.
    #[instrument(name = "horizon.stream_payments", skip(self, tx), fields(stellar.account = %account))]
    pub async fn stream_payments(
        &self,
        account: &str,
        tx: mpsc::Sender<Result<StreamPayment, HorizonError>>,
        initial_cursor: Option<String>,
    ) -> Result<(), HorizonError> {
        let mut last_cursor = initial_cursor;
        // u32 so .min(5) is always safe and we never approach the shift limit.
        let mut reconnect_count: u32 = 0;
        let metrics = Arc::new(tokio::sync::Mutex::new(StreamMetrics {
            reconnections: 0,
            events_received: 0,
            last_event_time: None,
        }));

        loop {
            let mut url = format!(
                "{}/accounts/{}/payments?order=asc&stream=true",
                self.base_url.trim_end_matches('/'),
                account
            );
            if let Some(ref cursor) = last_cursor {
                url.push_str(&format!("&cursor={}", cursor));
            }

            match self.connect_stream(&url, &tx, &metrics).await {
                Ok((events_in_session, new_cursor)) => {
                    if let Some(c) = new_cursor {
                        last_cursor = Some(c);
                    }

                    // A session that delivered at least one event is "healthy";
                    // reset backoff so a brief outage after a long healthy run
                    // doesn't impose unnecessary delay.
                    if events_in_session > 0 {
                        reconnect_count = 0;
                    } else {
                        reconnect_count = reconnect_count.saturating_add(1);
                    }

                    {
                        let mut m = metrics.lock().await;
                        m.reconnections += 1;
                    }

                    tracing::warn!(
                        account,
                        reconnect_count,
                        last_cursor = ?last_cursor,
                        "SSE stream disconnected, reconnecting"
                    );

                    // Cap exponent at 5: max shift is 1<<5 = 32, clamped to 30 s.
                    let backoff_secs = std::cmp::min(1u64 << reconnect_count.min(5), 30);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                }
                Err(e) => {
                    let _ = tx.send(Err(e.clone())).await;
                    return Err(e);
                }
            }
        }
    }

    /// Connect to an SSE stream and forward complete events to `tx`.
    ///
    /// Accumulates raw bytes across TCP chunks in a `String` buffer and only
    /// parses a payment once a complete SSE event (delimited by `\n\n`) is
    /// assembled. Multi-line `data:` fields are concatenated before parsing.
    ///
    /// Returns `(events_sent, last_cursor)`. `last_cursor` is the Horizon
    /// paging token (`payment.id`) of the last payment forwarded to `tx`.
    pub(crate) async fn connect_stream(
        &self,
        url: &str,
        tx: &mpsc::Sender<Result<StreamPayment, HorizonError>>,
        metrics: &Arc<tokio::sync::Mutex<StreamMetrics>>,
    ) -> Result<(u64, Option<String>), HorizonError> {
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
        let mut buf = String::new();
        let mut events_sent: u64 = 0;
        let mut last_cursor: Option<String> = None;

        while let Some(chunk) = stream.next().await {
            let chunk: bytes::Bytes = chunk?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Split on the SSE event delimiter. Only process complete events.
            while let Some(pos) = buf.find("\n\n") {
                let raw_event = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();

                let data = parse_sse_event(&raw_event);
                if data.is_empty() {
                    // Heartbeat or comment-only event.
                    continue;
                }

                match serde_json::from_str::<StreamPayment>(&data) {
                    Ok(payment) => {
                        {
                            let mut m = metrics.lock().await;
                            m.events_received += 1;
                            m.last_event_time = Some(std::time::Instant::now());
                        }

                        last_cursor = Some(payment.id.clone());

                        if tx.send(Ok(payment)).await.is_err() {
                            // Receiver dropped — shut down cleanly.
                            return Ok((events_sent, last_cursor));
                        }
                        events_sent += 1;
                    }
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            raw_event = %data,
                            "Unparseable SSE payment event; payment may be lost"
                        );
                        tracing::info!(counter.sse_parse_errors = 1u64);
                    }
                }
            }
        }

        Ok((events_sent, last_cursor))
    }
}

/// Assemble the `data` payload from a raw SSE event block (text between two
/// `\n\n` delimiters). Multi-line `data:` fields are concatenated in order.
/// Returns an empty string for heartbeat/comment-only events.
pub(crate) fn parse_sse_event(raw_event: &str) -> String {
    let mut data = String::new();
    for line in raw_event.lines() {
        if let Some(value) = line.strip_prefix("data: ") {
            data.push_str(value);
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
        }
    }
    data
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

    // === Horizon stream resumption tests

    #[test]
    fn test_backoff_never_overflows_past_64_reconnects() {
        for count in 0u32..=100 {
            let backoff = std::cmp::min(1u64 << count.min(5), 30);
            assert!(backoff <= 30, "backoff exceeded 30 s at count={count}");
        }
    }

    #[test]
    fn test_parse_sse_event_complete_single_line() {
        let raw = r#"data: {"id":"p1","from":"G1","to":"G2","amount":"10","asset_code":"USD","memo":null,"memo_type":null,"created_at":"2024-01-01"}"#;
        let data = parse_sse_event(raw);
        assert!(!data.is_empty());
        let payment = serde_json::from_str::<StreamPayment>(&data).unwrap();
        assert_eq!(payment.id, "p1");
    }

    #[test]
    fn test_parse_sse_event_heartbeat_returns_empty() {
        assert_eq!(parse_sse_event(": heartbeat"), "");
        assert_eq!(parse_sse_event(""), "");
    }

    #[test]
    fn test_sse_buffer_reassembles_chunk_split_event() {
        let payment_json = r#"{"id":"split-1","from":"GA","to":"GB","amount":"5","asset_code":"EUR","memo":null,"memo_type":null,"created_at":"2024-06-01"}"#;

        // Split the SSE event across two chunks at an arbitrary byte boundary.
        let full_event = format!("data: {}\n\n", payment_json);
        let split_at = full_event.len() / 2;
        let chunk1 = &full_event[..split_at];
        let chunk2 = &full_event[split_at..];

        let mut buf = String::new();
        let mut parsed: Vec<StreamPayment> = Vec::new();

        // Simulate chunk1 arriving: no complete event yet.
        buf.push_str(chunk1);
        while let Some(pos) = buf.find("\n\n") {
            let raw = buf[..pos].to_string();
            buf = buf[pos + 2..].to_string();
            let data = parse_sse_event(&raw);
            if !data.is_empty() {
                parsed.push(serde_json::from_str(&data).unwrap());
            }
        }
        assert!(parsed.is_empty(), "no complete event after chunk1 alone");

        // Simulate chunk2 completing the event.
        buf.push_str(chunk2);
        while let Some(pos) = buf.find("\n\n") {
            let raw = buf[..pos].to_string();
            buf = buf[pos + 2..].to_string();
            let data = parse_sse_event(&raw);
            if !data.is_empty() {
                parsed.push(serde_json::from_str(&data).unwrap());
            }
        }

        assert_eq!(parsed.len(), 1, "exactly one event after both chunks");
        assert_eq!(parsed[0].id, "split-1");
    }

    #[tokio::test]
    async fn test_stream_resumes_with_cursor_after_disconnect() {
        let mut server = mockito::Server::new_async().await;

        let p1 = r#"{"id":"cursor-1","from":"GA","to":"GB","amount":"10","asset_code":"USD","memo":null,"memo_type":null,"created_at":"2024-01-01"}"#;
        let p2 = r#"{"id":"cursor-2","from":"GA","to":"GB","amount":"20","asset_code":"USD","memo":null,"memo_type":null,"created_at":"2024-01-02"}"#;

        // First connection (no cursor): returns p1 then closes.
        let _mock1 = server
            .mock("GET", "/accounts/GTEST/payments?order=asc&stream=true")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(format!("data: {}\n\n", p1))
            .create_async()
            .await;

        // Second connection (cursor=cursor-1): returns p2 then closes.
        let _mock2 = server
            .mock(
                "GET",
                "/accounts/GTEST/payments?order=asc&stream=true&cursor=cursor-1",
            )
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(format!("data: {}\n\n", p2))
            .create_async()
            .await;

        let client = HorizonClient::new(server.url());
        let metrics = Arc::new(tokio::sync::Mutex::new(StreamMetrics {
            reconnections: 0,
            events_received: 0,
            last_event_time: None,
        }));
        let (tx, mut rx) = mpsc::channel(10);

        // First session: no cursor.
        let url1 = format!(
            "{}/accounts/GTEST/payments?order=asc&stream=true",
            server.url()
        );
        let (n1, cursor1) = client.connect_stream(&url1, &tx, &metrics).await.unwrap();
        assert_eq!(n1, 1, "first session must deliver p1");
        assert_eq!(cursor1.as_deref(), Some("cursor-1"));

        // Second session: resume from cursor-1.
        let url2 = format!(
            "{}/accounts/GTEST/payments?order=asc&stream=true&cursor=cursor-1",
            server.url()
        );
        let (n2, cursor2) = client.connect_stream(&url2, &tx, &metrics).await.unwrap();
        assert_eq!(n2, 1, "second session must deliver p2");
        assert_eq!(cursor2.as_deref(), Some("cursor-2"));

        drop(tx);

        let mut received_ids: Vec<String> = Vec::new();
        while let Some(Ok(p)) = rx.recv().await {
            received_ids.push(p.id.clone());
        }

        assert_eq!(
            received_ids,
            vec!["cursor-1", "cursor-2"],
            "no gaps, no duplicates"
        );
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
