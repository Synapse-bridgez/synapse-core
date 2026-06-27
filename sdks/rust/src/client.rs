use crate::error::SynapseError;
use crate::retry::{retry_with_backoff, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS};
use serde::de::DeserializeOwned;

/// HTTP client for the Synapse public API.
///
/// Construct via [`SynapseClient::new`] or [`SynapseClient::builder`]. All
/// requests are issued with the configured API key and are retried
/// automatically on transient failures.
#[derive(Clone)]
pub struct SynapseClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) max_attempts: u32,
    pub(crate) base_delay_ms: u64,
}

/// Builder for [`SynapseClient`].
pub struct SynapseClientBuilder {
    base_url: String,
    api_key: String,
    max_attempts: u32,
    base_delay_ms: u64,
}

impl SynapseClient {
    /// Construct a client with default retry settings.
    ///
    /// Equivalent to `SynapseClient::builder(base_url, api_key).build()`.
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
        }
    }

    /// Issue an authenticated GET request to `path` and deserialize the JSON response.
    ///
    /// The request is retried automatically according to the client's retry
    /// configuration. 4xx responses are returned immediately without retrying.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        self.get_query(path, &[]).await
    }

    /// Issue an authenticated GET request to `path` with query parameters and
    /// deserialize the JSON response.
    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SynapseError> {
        let base_url = format!("{}{}", self.base_url, path);
        let url = if query.is_empty() {
            base_url
        } else {
            let qs: String = query
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}={}",
                        urlencoding_simple(k),
                        urlencoding_simple(v)
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{base_url}?{qs}")
        };
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                let resp = http
                    .get(&url)
                    .header("X-API-Key", &key)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.json::<T>().await.map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated GET request and return the raw response bytes
    /// (no JSON parsing). Useful for binary/CSV responses.
    pub async fn get_bytes(&self, path: &str, query: &[(&str, &str)]) -> Result<Vec<u8>, SynapseError> {
        let base_url = format!("{}{}", self.base_url, path);
        let url = if query.is_empty() {
            base_url
        } else {
            let qs: String = query
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding_simple(k), urlencoding_simple(v)))
                .collect::<Vec<_>>()
                .join("&");
            format!("{base_url}?{qs}")
        };
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                let resp = http
                    .get(&url)
                    .header("X-API-Key", &key)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.bytes().await.map(|b| b.to_vec()).map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated POST request with a JSON body and deserialize the JSON response.
    pub async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        let body_bytes = serde_json::to_vec(body).map_err(|e| SynapseError::Api {
            status: 0,
            message: e.to_string(),
        })?;
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            let body_bytes = body_bytes.clone();
            async move {
                let resp = http
                    .post(&url)
                    .header("X-API-Key", &key)
                    .header("Content-Type", "application/json")
                    .body(body_bytes)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.json::<T>().await.map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Return a handle to the `transactions` resource.
    pub fn transactions(&self) -> crate::resources::transactions::Transactions<'_> {
        crate::resources::transactions::Transactions { client: self }
    }

    /// Return a handle to the `settlements` resource.
    pub fn settlements(&self) -> crate::resources::settlements::Settlements<'_> {
        crate::resources::settlements::Settlements { client: self }
    }

    /// Return a handle to the `events` resource.
    pub fn events(&self) -> crate::resources::events::Events<'_> {
        crate::resources::events::Events { client: self }
    }
}

/// Percent-encode a query string key or value (spaces → %20, etc.).
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

impl SynapseClientBuilder {
    /// Set the maximum total number of attempts, including the first (default: 3).
    ///
    /// Values below 1 are treated as 1 (no retries).
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// Disable retry behaviour. The first failure is returned immediately.
    ///
    /// Use this when the caller manages its own retry loop.
    pub fn disable_retries(mut self) -> Self {
        self.max_attempts = 1;
        self
    }

    /// Set the base delay in milliseconds for exponential backoff (default: 200).
    pub fn base_delay_ms(mut self, ms: u64) -> Self {
        self.base_delay_ms = ms;
        self
    }

    /// Build the [`SynapseClient`].
    pub fn build(self) -> SynapseClient {
        SynapseClient {
            http: reqwest::Client::new(),
            base_url: self.base_url,
            api_key: self.api_key,
            max_attempts: self.max_attempts,
            base_delay_ms: self.base_delay_ms,
        }
    }
}
