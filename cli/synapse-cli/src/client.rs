use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

// ── ApiClient ─────────────────────────────────────────────────────────────────
// Shared HTTP client used by most commands (health, stats, events, settlements,
// transactions, webhooks, …). Sends X-API-Key on every request and surfaces
// non-2xx responses as errors.

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

    /// `GET <base_url><path>` — deserialise JSON body into `T`.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .context("request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }

        resp.json::<T>()
            .await
            .context("failed to parse response JSON")
    }

    /// `GET <base_url><path>?key=value&…` — deserialise JSON body into `T`.
    pub async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query_params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url).header("X-API-Key", &self.api_key);
        for (k, v) in query_params {
            req = req.query(&[(k, v)]);
        }
        let resp = req.send().await.context("request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }

        resp.json::<T>()
            .await
            .context("failed to parse response JSON")
    }

    /// `POST <base_url><path>` with a JSON body — deserialise JSON response into `T`.
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .post(&url)
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .context("request failed")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body_text);
        }

        resp.json::<T>()
            .await
            .context("failed to parse response JSON")
    }

    /// `GET <base_url><path>` returning the raw status code and body text,
    /// without attempting to interpret non-2xx as an error. Used by callers
    /// that need to distinguish specific status codes (e.g. 404) before
    /// deciding how to report a failure.
    pub async fn get_raw_status(&self, path: &str) -> Result<(u16, String)> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .context("request failed")?;

        let status = resp.status().as_u16();
        let body = resp.text().await.context("failed to read response body")?;
        Ok((status, body))
    }
}

// ── SynapseCliClient ──────────────────────────────────────────────────────────
// Thin client used by the transactions/settlements/graphql top-level handlers.
// Unlike `ApiClient` it does not send an API key header.

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

    /// `GET <base_url><path>` — deserialise JSON body into `T`.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }
        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    /// `GET <base_url><path>?key=value&…` — deserialise JSON body into `T`.
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
        let response = req.send().await.context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }
        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    /// `GET <base_url><path>?…` returning the raw response bytes (used for
    /// CSV/JSON export downloads).
    pub async fn get_bytes(&self, path: &str, query_params: &[(&str, &str)]) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let response = req.send().await.context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("server returned {}: {}", status.as_u16(), body);
        }
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
    pub async fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .context("request failed")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {}: {}", status.as_u16(), text);
        }

        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }
}
