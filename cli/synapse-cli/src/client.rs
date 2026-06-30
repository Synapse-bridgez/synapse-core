use anyhow::Result;
use reqwest::Client;
use serde_json::Value;
use anyhow::{bail, Context, Result};

pub struct SynapseCliClient {
    client: reqwest::Client,
    base_url: String,
}

impl SynapseCliClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url).send().await?;
        response.json().await.map_err(|e| anyhow::anyhow!(e))
        self.send(self.client.get(self.url(path))).await
    }

    pub async fn get_with_query<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> Result<T> {
        self.send(self.client.get(self.url(path)).query(query_params))
            .await
    }

    pub async fn get_bytes(&self, path: &str, query_params: &[(&str, &str)]) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);

        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }

        let response = req.send().await?;
        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// POST a JSON body to `path` and deserialize the response as `T`.
    ///
    /// Returns an error for non-2xx HTTP status codes. On success the raw
    /// response body is deserialized — callers are responsible for inspecting
    /// the returned value for application-level GraphQL errors.
    pub async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.post(&url).json(body).send().await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status.as_u16(), text);
        }

        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }
}

/// Generic API client used by the health and stats command modules.
/// Sends requests with an `X-API-Key` header and surfaces non-2xx responses
/// as errors.
pub struct ApiClient {
    base_url: String,
    api_key: String,
    client: Client,
}

impl ApiClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: Client::new(),
        }
    }

    pub async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status.as_u16(), body);
        }

        resp.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    pub async fn get_with_query<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> anyhow::Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key);
        let response = self
            .client
            .get(self.url(path))
            .query(query_params)
            .send()
            .await
            .context("request failed")?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .context("failed to read response body")?;

        if !status.is_success() {
            bail!(
                "server returned {status}: {}",
                String::from_utf8_lossy(&bytes)
            );
        }

        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status.as_u16(), body);
        }

        resp.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }
}

/// Thin client used by older command modules that need per-request API-key
/// injection and typed error variants.
#[derive(Debug)]
pub enum ClientError {
    NotFound(String),
    Http { status: u16, body: String },
    Network(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotFound(msg) => write!(f, "Not found: {}", msg),
            ClientError::Http { status, body } => write!(f, "HTTP {}: {}", status, body),
            ClientError::Network(msg) => write!(f, "Network error: {}", msg),
        }
        Ok(bytes.to_vec())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Fetch a transaction by ID. Returns `NotFound` for 404, `Http` for other
    /// non-success statuses.
    pub async fn get_transaction(&self, id: &str) -> Result<Value, ClientError> {
        let url = format!("{}/transactions/{}", self.base_url, id);
        let client = reqwest::Client::new();

        let resp = client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let response = request.send().await.context("request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read response body")?;

        if !status.is_success() {
            bail!("server returned {status}: {body}");
        }

        serde_json::from_str(&body).context("failed to parse response JSON")
    }
}
