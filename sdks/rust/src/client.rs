use crate::error::{
    build_api_error, map_status_to_error, parse_api_error, CatalogEntry, CatalogResponse,
    SynapseError,
};
use crate::resources::admin::{
    AdminBulkStatus, AdminDlq, AdminLocks, AdminReconciliation, AdminSettlements,
    AdminWebhookReplay,
};
use crate::resources::health::Health;
use crate::resources::settlements::Settlements;
use crate::resources::transactions::Transactions;
use crate::retry::{retry_with_backoff, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::OnceCell;

/// Default maximum number of idle connections kept per host in the pool.
///
/// `reqwest`'s own default is unbounded, which lets a bursty client accumulate
/// idle sockets indefinitely. The SDK caps this at a sane value so long-lived
/// high-throughput services do not leak file descriptors.
pub const DEFAULT_POOL_MAX_IDLE_PER_HOST: usize = 32;

/// Default idle timeout for pooled connections (90 seconds).
///
/// Connections idle longer than this are closed and removed from the pool,
/// which keeps the pool from holding sockets that intermediaries (load
/// balancers, NAT gateways) have already silently dropped.
pub const DEFAULT_POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Default TCP keep-alive interval for pooled connections (60 seconds).
///
/// Enabling TCP keep-alive lets the OS probe idle sockets so that a connection
/// returned to the pool after a failed request is detected as dead before it is
/// reused, rather than surfacing as a spurious error on the next call.
pub const DEFAULT_TCP_KEEPALIVE: Duration = Duration::from_secs(60);

/// Parse a `Retry-After` response header (seconds, per RFC 9110 §10.2.3) into
/// milliseconds. Returns `None` if the header is absent or not a plain
/// integer (the HTTP-date form is not handled).
fn parse_retry_after_ms(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|secs| secs.saturating_mul(1000))
}

/// HTTP client for the Synapse public API.
///
/// Construct via [`SynapseClient::new`] or [`SynapseClient::builder`]. All
/// requests are issued with the configured API key and are retried automatically
/// on transient failures.
#[derive(Clone)]
pub struct SynapseClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) max_attempts: u32,
    pub(crate) base_delay_ms: u64,
    pub(crate) catalog: Arc<OnceCell<HashMap<String, CatalogEntry>>>,
}

/// Builder for [`SynapseClient`].
pub struct SynapseClientBuilder {
    base_url: String,
    api_key: String,
    max_attempts: u32,
    base_delay_ms: u64,
    pool_max_idle_per_host: usize,
    pool_idle_timeout: Option<Duration>,
    tcp_keepalive: Option<Duration>,
}

impl SynapseClient {
    /// Create a new [`SynapseClient`] with default retry settings.
    ///
    /// This is a convenience method equivalent to `SynapseClient::builder(url, key).build()`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::builder(base_url, api_key).build()
    }

    /// Return a builder for constructing a [`SynapseClient`].
    pub fn builder(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> SynapseClientBuilder {
        SynapseClientBuilder {
            base_url: base_url.into(),
            api_key: api_key.into(),
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay_ms: DEFAULT_BASE_DELAY_MS,
            pool_max_idle_per_host: DEFAULT_POOL_MAX_IDLE_PER_HOST,
            pool_idle_timeout: Some(DEFAULT_POOL_IDLE_TIMEOUT),
            tcp_keepalive: Some(DEFAULT_TCP_KEEPALIVE),
        }
    }

    /// Access the transactions resource.
    pub fn transactions(&self) -> Transactions<'_> {
        Transactions { client: self }
    }

    /// Access the settlements resource.
    pub fn settlements(&self) -> Settlements<'_> {
        Settlements { client: self }
    }

    /// Access the health resource.
    pub fn health(&self) -> Health<'_> {
        Health { client: self }
    }

    /// Access the graphql resource.
    pub fn graphql(&self) -> crate::resources::graphql::GraphQL<'_> {
        crate::resources::graphql::GraphQL { client: self }
    }

    /// Access the stats resource.
    pub fn stats(&self) -> crate::resources::stats::Stats<'_> {
        crate::resources::stats::Stats { client: self }
    }

    /// Access the events resource.
    pub fn events(&self) -> crate::resources::events::Events<'_> {
        crate::resources::events::Events { client: self }
    }

    fn build_url(&self, path: &str, query: &[(&str, &str)]) -> String {
        let base = format!("{}{}", self.base_url, path);
        if query.is_empty() {
            base
        } else {
            let mut url = url::Url::parse(&base).expect("base_url must be a valid URL");
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in query {
                    pairs.append_pair(k, v);
                }
            }
            url.into()
        }
    }

    async fn get_response(&self, path: &str) -> Result<reqwest::Response, SynapseError> {
        let url = self.build_url(path, &[]);
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                http.get(&url)
                    .header("X-API-Key", &key)
                    .send()
                    .await
                    .map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated GET request to `path` and deserialize the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        self.get_query(path, &[]).await
    }

    /// Issue an authenticated GET request with query parameters and deserialize the JSON response.
    ///
    /// The request is retried automatically according to the client's retry
    /// configuration. 4xx responses are returned immediately without retrying.
    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let raw = retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            let query = query.clone();
            async move {
                let resp = http
                    .get(&url)
                    .query(&query)
                    .header("X-API-Key", &key)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let retry_after_ms = parse_retry_after_ms(&resp);
                    let body = resp.text().await.unwrap_or_default();
                    return Err(match retry_after_ms {
                        Some(retry_after_ms) => SynapseError::HttpRetryAfter {
                            status,
                            body,
                            retry_after_ms,
                        },
                        None => SynapseError::Http { status, body },
                    });
                }
                resp.json::<T>()
                    .await
                    .map_err(|e| SynapseError::Decode(e.to_string()))
            }
        })
        .await;
        match raw {
            Err(SynapseError::Http { status, body }) => Err(self.map_api_error(status, body).await),
            Err(SynapseError::HttpRetryAfter { status, body, .. }) => {
                Err(self.map_api_error(status, body).await)
            }
            other => other,
        }
    }

    /// Issue an authenticated POST request with a JSON body and deserialize the JSON response.
    ///
    /// POST is a mutating, non-idempotent request: unlike GET, it is **never**
    /// auto-retried on transient failure, since a lost response after a
    /// successful server-side write would otherwise be silently resent as a
    /// duplicate. If you need retries for a mutating call, wrap it in your
    /// own retry loop with an idempotency key your application controls.
    pub async fn post<T: DeserializeOwned, B: Serialize + Clone + Send + 'static>(
        &self,
        path: &str,
        body: B,
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        let raw = retry_with_backoff(1, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            let body = body.clone();
            async move {
                let resp = http
                    .post(&url)
                    .header("X-API-Key", &key)
                    .json(&body)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let retry_after_ms = parse_retry_after_ms(&resp);
                    let body = resp.text().await.unwrap_or_default();
                    return Err(match retry_after_ms {
                        Some(retry_after_ms) => SynapseError::HttpRetryAfter {
                            status,
                            body,
                            retry_after_ms,
                        },
                        None => SynapseError::Http { status, body },
                    });
                }
                resp.json::<T>()
                    .await
                    .map_err(|e| SynapseError::Decode(e.to_string()))
            }
        })
        .await;
        match raw {
            Err(SynapseError::Http { status, body }) => Err(self.map_api_error(status, body).await),
            Err(SynapseError::HttpRetryAfter { status, body, .. }) => {
                Err(self.map_api_error(status, body).await)
            }
            other => other,
        }
    }

    async fn map_api_error(&self, status: u16, body: String) -> SynapseError {
        let _ = map_status_to_error(status);
        let _ = parse_api_error(&body);
        let _ = build_api_error(status, &body);
        SynapseError::Http { status, body }
    }
}

impl SynapseClientBuilder {
    /// Set the maximum number of idle connections retained per host in the
    /// connection pool.
    ///
    /// For high-throughput services issuing many concurrent SDK calls, raising
    /// this (e.g. to `128`) lets the pool keep more warm sockets to the API
    /// host, avoiding repeated TCP/TLS handshakes. Lower it to bound the number
    /// of idle file descriptors a long-lived process holds.
    pub fn pool_max_idle_per_host(mut self, max_idle_per_host: usize) -> Self {
        self.pool_max_idle_per_host = max_idle_per_host;
        self
    }

    /// Set how long an idle pooled connection is kept before being closed.
    ///
    /// Pass `None` to disable the idle timeout (connections are kept until the
    /// peer or an intermediary closes them). The default is
    /// [`DEFAULT_POOL_IDLE_TIMEOUT`].
    pub fn pool_idle_timeout(mut self, idle_timeout: Option<Duration>) -> Self {
        self.pool_idle_timeout = idle_timeout;
        self
    }

    /// Set the TCP keep-alive interval for pooled connections.
    ///
    /// TCP keep-alive probes idle sockets so that a connection returned to the
    /// pool after a failed request is detected as dead before it is reused,
    /// rather than surfacing as a spurious error on the next call. Pass `None`
    /// to disable TCP keep-alive. The default is [`DEFAULT_TCP_KEEPALIVE`].
    pub fn tcp_keepalive(mut self, keepalive: Option<Duration>) -> Self {
        self.tcp_keepalive = keepalive;
        self
    }

    /// Set the maximum number of attempts for retryable (idempotent) requests.
    pub fn max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Set the base delay (in milliseconds) used for exponential backoff.
    pub fn base_delay_ms(mut self, base_delay_ms: u64) -> Self {
        self.base_delay_ms = base_delay_ms;
        self
    }

    /// Build the [`SynapseClient`], applying the configured connection-pooling
    /// and retry settings to the underlying HTTP client.
    pub fn build(self) -> SynapseClient {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(self.pool_max_idle_per_host)
            .pool_idle_timeout(self.pool_idle_timeout)
            .tcp_keepalive(self.tcp_keepalive)
            .build()
            .expect("failed to build HTTP client");
        SynapseClient {
            http,
            base_url: self.base_url,
            api_key: self.api_key,
            max_attempts: self.max_attempts,
            base_delay_ms: self.base_delay_ms,
            catalog: Arc::new(OnceCell::new()),
        }
    }
}
