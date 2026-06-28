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

        Ok(bytes.to_vec())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

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
