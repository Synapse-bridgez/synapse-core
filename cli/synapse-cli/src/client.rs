use anyhow::{bail, Result};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

// ── ApiClient ─────────────────────────────────────────────────────────────────
// Shared HTTP client used by all commands (health, stats, events, …).
// Sends X-API-Key on every request and surfaces non-2xx responses as errors.

pub struct ApiClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let base_url = base_url.into();
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    /// `GET <base_url><path>` — deserialise JSON body into `T`.
    pub async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
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
            bail!("server returned {}: {}", status.as_u16(), body);
        }
        resp.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    /// `GET <base_url><path>?key=value&…` — deserialise JSON body into `T`.
    pub async fn get_with_query<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key);
        for (k, v) in query_params {
            req = req.query(&[(k, v)]);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }
        resp.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    /// `POST <base_url><path>` with a JSON body — deserialise JSON response into `T`.
    pub async fn post<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body_text);
        }
        resp.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }
}

// ── SynapseCliClient ──────────────────────────────────────────────────────────
// Legacy client used by the transactions/settlements handlers; kept for
// backward compatibility.

pub struct SynapseCliClient {
    client: Client,
    base_url: String,
}

impl SynapseCliClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url).send().await?;
        response.json().await.map_err(|e| anyhow::anyhow!(e))
    }

    pub async fn get_with_query<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let response = req.send().await?;
        response.json().await.map_err(|e| anyhow::anyhow!(e))
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
}

// ── SynapseApiClient ──────────────────────────────────────────────────────────
// Thin client used by the transactions `get` handler.

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
    }
}

pub struct SynapseApiClient {
    base_url: String,
    api_key: String,
}

impl SynapseApiClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    /// Fetch a transaction by ID. Returns `NotFound` for 404, `Http` for other errors.
    pub async fn get_transaction(&self, id: &str) -> Result<Value, ClientError> {
        let url = format!("{}/transactions/{}", self.base_url, id);
        let client = reqwest::Client::new();

        let resp = client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))?;

        let status = resp.status().as_u16();

        if status == 404 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::NotFound(body));
        }

        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::Http { status, body });
        }

        resp.json::<Value>()
            .await
            .map_err(|e| ClientError::Network(e.to_string()))
    }
}
