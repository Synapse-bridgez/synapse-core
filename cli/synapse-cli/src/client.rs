use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;

// ── ApiClient (used by commands::stats) ──────────────────────────────────────

/// Thin HTTP client used by all stats subcommands.
pub struct ApiClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl ApiClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
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
            return Err(anyhow!("HTTP {}: {}", status.as_u16(), body));
        }
        resp.json::<T>().await.map_err(|e| anyhow!(e))
    }

    pub async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key);
        for (k, v) in query {
            req = req.query(&[(k, v)]);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("HTTP {}: {}", status.as_u16(), body));
        }
        resp.json::<T>().await.map_err(|e| anyhow!(e))
    }
}

// ── SynapseCliClient (used by handlers in main.rs) ────────────────────────────

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

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self.client.get(&url).send().await?;
        resp.json().await.map_err(|e| anyhow!(e))
    }

    pub async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let resp = req.send().await?;
        resp.json().await.map_err(|e| anyhow!(e))
    }

    pub async fn get_bytes(&self, path: &str, query_params: &[(&str, &str)]) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let resp = req.send().await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| anyhow!(e))
    }
}

// ── SynapseApiClient (used by transactions get) ───────────────────────────────

use serde_json::Value;

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

    /// Fetch a transaction by ID. Returns `NotFound` for HTTP 404.
    pub async fn get_transaction(&self, id: &str) -> std::result::Result<Value, ClientError> {
        let url = format!("{}/transactions/{}", self.base_url, id);
        let client = Client::new();

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
