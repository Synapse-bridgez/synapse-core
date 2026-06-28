use crate::error::{
    map_status_to_error, parse_api_error, CatalogEntry, CatalogResponse, SynapseError,
};
use crate::retry::retry_with_backoff;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::OnceCell;

/// HTTP client for admin-only Synapse API endpoints.
///
/// Obtain via [`crate::SynapseClient::as_admin`]. Sends
/// `Authorization: Bearer <admin_key>` on every request, mirroring the
/// server's `admin_auth` middleware. Public resource methods are not
/// available on this type, preventing accidental mix-up of admin and
/// public-API scopes.
#[derive(Clone)]
pub struct AdminClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) admin_key: String,
    pub(crate) max_attempts: u32,
    pub(crate) base_delay_ms: u64,
    pub(crate) catalog: Arc<OnceCell<HashMap<String, CatalogEntry>>>,
}

impl AdminClient {
    pub(crate) fn new(
        http: reqwest::Client,
        base_url: String,
        admin_key: String,
        max_attempts: u32,
        base_delay_ms: u64,
        catalog: Arc<OnceCell<HashMap<String, CatalogEntry>>>,
    ) -> Self {
        Self {
            http,
            base_url,
            admin_key,
            max_attempts,
            base_delay_ms,
            catalog,
        }
    }

    /// Issue an admin-authenticated GET request to `path` and deserialize the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.admin_key.clone();
        let http = self.http.clone();
        let raw = retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                let resp = http
                    .get(&url)
                    .header("Authorization", format!("Bearer {}", key))
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Http { status, body });
                }
                resp.json::<T>().await.map_err(|e| SynapseError::Decode(e.to_string()))
            }
        })
        .await;
        match raw {
            Err(SynapseError::Http { status, body }) => Err(self.map_api_error(status, body).await),
            other => other,
        }
    }

    /// Issue an admin-authenticated GET request with query parameters and deserialize the JSON response.
    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.admin_key.clone();
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
                    .header("Authorization", format!("Bearer {}", key))
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Http { status, body });
                }
                resp.json::<T>().await.map_err(|e| SynapseError::Decode(e.to_string()))
            }
        })
        .await;
        match raw {
            Err(SynapseError::Http { status, body }) => Err(self.map_api_error(status, body).await),
            other => other,
        }
    }

    /// Fetch `/errors` on first call and return a reference to the cached catalog.
    async fn ensure_catalog(&self) -> Option<&HashMap<String, CatalogEntry>> {
        let http = self.http.clone();
        let url = format!("{}/errors", self.base_url);
        self.catalog
            .get_or_try_init(|| async move {
                let resp = http.get(&url).send().await?;
                let body: CatalogResponse = resp.json().await?;
                let map = body
                    .errors
                    .into_iter()
                    .map(|e| (e.code.clone(), e))
                    .collect();
                Ok::<_, reqwest::Error>(map)
            })
            .await
            .ok()
    }

    /// Translate a raw HTTP error into a typed [`SynapseError`] using the
    /// lazily-fetched error catalog. Unknown codes fall back to [`SynapseError::Api`].
    async fn map_api_error(&self, status: u16, body: String) -> SynapseError {
        let (code, base_msg) = parse_api_error(&body);
        let is_named = matches!(status, 401 | 403 | 404 | 429);
        let description = if is_named {
            match &code {
                Some(c) => self
                    .ensure_catalog()
                    .await
                    .and_then(|cat| cat.get(c))
                    .map(|e| e.description.clone()),
                None => None,
            }
        } else {
            None
        };
        let message = description.unwrap_or(base_msg);
        map_status_to_error(status, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SynapseClient;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn catalog_body() -> serde_json::Value {
        serde_json::json!({
            "errors": [
                {"code": "ERR_AUTH_001", "http_status": 401, "description": "Invalid authentication credentials"},
                {"code": "ERR_NOT_FOUND_001", "http_status": 404, "description": "Resource not found"}
            ],
            "version": "1.0.0"
        })
    }

    #[tokio::test]
    async fn admin_get_sends_bearer_token() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/errors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog_body()))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/admin/status"))
            .and(header("Authorization", "Bearer admin-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "public-key");
        let admin = client.as_admin("admin-secret");
        let result: Result<serde_json::Value, _> = admin.get("/admin/status").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn admin_get_returns_unauthorized_on_401_with_known_code() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/errors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog_body()))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/admin/secret"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": "Invalid authentication credentials",
                    "code": "ERR_AUTH_001",
                    "status": 401
                })),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "public-key");
        let admin = client.as_admin("wrong-key");
        let result: Result<serde_json::Value, _> = admin.get("/admin/secret").await;

        assert!(
            matches!(result, Err(SynapseError::Unauthorized(_))),
            "expected Unauthorized, got: {:?}",
            result
        );
    }
}
