use crate::client::SynapseClient;
use crate::error::SynapseError;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub db: String,
    pub db_pool: serde_json::Value,
    pub pending_queue_depth: u64,
    pub current_batch_size: u64,
    pub ws_connection_count: usize,
}

pub struct Health<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Health<'a> {
    /// Fetch the `/health` status from the Synapse API.
    ///
    /// If the service reports an unhealthy dependency, the response body is
    /// still returned as data along with its HTTP status.
    pub async fn health(&self) -> Result<(u16, HealthResponse), SynapseError> {
        self.client.get_json_with_status("/health").await
    }

    /// Fetch the `/live` probe response.
    pub async fn live(&self) -> Result<(u16, serde_json::Value), SynapseError> {
        self.client.get_json_with_status("/live").await
    }

    /// Fetch the `/ready` probe response.
    ///
    /// `ready()` may legitimately return a non-2xx status when the service is
    /// not currently accepting traffic. That response is returned as data,
    /// not as an exception.
    pub async fn ready(&self) -> Result<(u16, serde_json::Value), SynapseError> {
        self.client.get_json_with_status("/ready").await
    }

    /// Fetch the `/errors` error catalog.
    pub async fn errors(&self) -> Result<(u16, serde_json::Value), SynapseError> {
        self.client.get_json_with_status("/errors").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn live_returns_json_payload() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/live"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "alive"
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let (status, body) = client.health().live().await.unwrap();

        assert_eq!(status, 200);
        assert_eq!(body["status"], "alive");
    }

    #[tokio::test]
    async fn ready_returns_non_2xx_as_data() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ready"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
                "status": "not_ready",
                "draining": true
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let (status, body) = client.health().ready().await.unwrap();

        assert_eq!(status, 503);
        assert_eq!(body["status"], "not_ready");
        assert_eq!(body["draining"], true);
    }
}
