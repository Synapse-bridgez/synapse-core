use anyhow::{Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

// ── ApiClient ─────────────────────────────────────────────────────────────────
pub use synapse_sdk::SynapseClient as ApiClient;

// ── SynapseCliClient ──────────────────────────────────────────────────────────
// Thin client used by the transactions/settlements/graphql top-level handlers.
// Sends the same `X-API-Key` header as `ApiClient` on every request.

pub struct SynapseCliClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl SynapseCliClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// `GET <base_url><path>` — deserialise JSON body into `T`.
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .header("X-API-Key", &self.api_key)
            .send()
            .await
            .context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(crate::error::map_http_error(status.as_u16(), body).into());
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
        let mut req = self.client.get(&url).header("X-API-Key", &self.api_key);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let response = req.send().await.context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(crate::error::map_http_error(status.as_u16(), body).into());
        }
        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }

    /// `GET <base_url><path>?…` returning the raw response bytes (used for
    /// CSV/JSON export downloads).
    pub async fn get_bytes(&self, path: &str, query_params: &[(&str, &str)]) -> Result<Vec<u8>> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.client.get(&url).header("X-API-Key", &self.api_key);
        for (key, value) in query_params {
            req = req.query(&[(key, value)]);
        }
        let response = req.send().await.context("request failed")?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(crate::error::map_http_error(status.as_u16(), body).into());
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
            .header("X-API-Key", &self.api_key)
            .json(body)
            .send()
            .await
            .context("request failed")?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(crate::error::map_http_error(status.as_u16(), text).into());
        }

        response.json::<T>().await.map_err(|e| anyhow::anyhow!(e))
    }
}
