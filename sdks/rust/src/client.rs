use crate::error::SynapseError;
use crate::resources::settlements::Settlements;
use crate::admin::AdminClient;
use crate::error::{
    map_status_to_error, parse_api_error, CatalogEntry, CatalogResponse, SynapseError,
};
use crate::resources::transactions::Transactions;
use crate::retry::{retry_with_backoff, DEFAULT_BASE_DELAY_MS, DEFAULT_MAX_ATTEMPTS};
use crate::resources::{health::Health, transactions::Transactions};
use serde::de::DeserializeOwned;
use serde::Serialize;

/// HTTP client for the Synapse public API.
///
/// Construct via [`SynapseClient::new`] or [`SynapseClient::builder`]. All
/// requests are issued with the configured API key and are retried automatically
/// on transient failures.
/// requests are issued with the configured API key and are retried
/// automatically on transient failures.
#[derive(Clone)]
pub struct SynapseClient {
    pub(crate) http: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) api_key: String,
    pub(crate) admin_key: Option<String>,
    pub(crate) max_attempts: u32,
    pub(crate) base_delay_ms: u64,
    pub(crate) catalog: Arc<OnceCell<HashMap<String, CatalogEntry>>>,
}

/// Builder for [`SynapseClient`].
pub struct SynapseClientBuilder {
    base_url: String,
    api_key: String,
    admin_key: Option<String>,
    max_attempts: u32,
    base_delay_ms: u64,
}

impl SynapseClient {
    /// Create a client with the given base URL and API key.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> SynapseClient {
        SynapseClient::builder(base_url, api_key).build()
    /// Create a new [`SynapseClient`] with default retry settings.
    ///
    /// Equivalent to `SynapseClient::builder(base_url, api_key).build()`.
    /// Convenience constructor; equivalent to `SynapseClient::builder(base_url, api_key).build()`.
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::builder(base_url, api_key).build()
    }

    /// Access the transactions resource.
    pub fn transactions(&self) -> Transactions<'_> {
        Transactions { client: self }
    }

    /// Access the settlements resource.
    pub fn settlements(&self) -> Settlements<'_> {
        Settlements { client: self }
    }

    /// Return a builder for constructing a [`SynapseClient`].
    pub fn builder(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> SynapseClientBuilder {
        SynapseClientBuilder {
            base_url: base_url.into(),
            api_key: api_key.into(),
            admin_key: None,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            base_delay_ms: DEFAULT_BASE_DELAY_MS,
        }
    }

    /// Return a handle for transaction endpoints.
    pub fn transactions(&self) -> Transactions {
        Transactions { client: self }
    }

    /// Return a handle for health endpoints.
    pub fn health(&self) -> Health {
        Health { client: self }
    }

    fn build_url(&self, path: &str, query: &[(&str, &str)]) -> String {
        if query.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            let query = query
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join("&");
            format!("{}{}?{}", self.base_url, path, query)
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
            async move { http.get(&url).header("X-API-Key", &key).send().await.map_err(SynapseError::Network) }
        })
        .await
    /// Construct a client with the given base URL and API key (no retries configured).
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        SynapseClient::builder(base_url, api_key).build()
    }

    /// Issue an authenticated GET request to `path` and deserialize the JSON response.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        self.get_query(path, &[]).await
    }

    /// Issue an authenticated GET request with query parameters.
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
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
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
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Api { status, message: body });
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

    /// Return a handle for the transactions resource.
    pub fn transactions(&self) -> Transactions<'_> {
        Transactions { client: self }
    }

    /// Return an [`AdminClient`] that authenticates with `admin_key` via
    /// `Authorization: Bearer`, sharing the underlying HTTP connection pool.
    pub fn as_admin(&self, admin_key: impl Into<String>) -> AdminClient {
        AdminClient::new(
            self.http.clone(),
            self.base_url.clone(),
            admin_key.into(),
            self.max_attempts,
            self.base_delay_ms,
            Arc::clone(&self.catalog),
        )
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
    ///
    /// Catalog descriptions are used only for named variants (401, 403, 404,
    /// 429). For all other statuses the body message is preserved as-is so
    /// that callers which inspect the message (e.g. cursor-error detection)
    /// continue to work.
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
    use wiremock::matchers::{method, path};
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

    fn mount_catalog(server: &MockServer) -> impl std::future::Future<Output = ()> + '_ {
        Mock::given(method("GET"))
            .and(path("/errors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(catalog_body()))
            .mount(server)
    }

    #[tokio::test]
    async fn maps_401_with_known_code_to_unauthorized() {
        let server = MockServer::start().await;
        mount_catalog(&server).await;

        Mock::given(method("GET"))
            .and(path("/protected"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "Unauthorized",
                "code": "ERR_AUTH_001",
                "status": 401
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "bad-key");
        let result: Result<serde_json::Value, _> = client.get("/protected").await;

        assert!(
            matches!(result, Err(SynapseError::Unauthorized(_))),
            "expected Unauthorized, got: {:?}",
            result
        );
        if let Err(SynapseError::Unauthorized(msg)) = result {
            assert_eq!(msg, "Invalid authentication credentials", "should use catalog description");
        }
    }

    #[tokio::test]
    async fn maps_404_with_known_code_to_not_found() {
        let server = MockServer::start().await;
        mount_catalog(&server).await;

        Mock::given(method("GET"))
            .and(path("/things/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": "Not found",
                "code": "ERR_NOT_FOUND_001",
                "status": 404
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "key");
        let result: Result<serde_json::Value, _> = client.get("/things/missing").await;

        assert!(
            matches!(result, Err(SynapseError::NotFound(_))),
            "expected NotFound, got: {:?}",
            result
        );
        if let Err(SynapseError::NotFound(msg)) = result {
            assert_eq!(msg, "Resource not found", "should use catalog description");
        }
    }

    #[tokio::test]
    async fn maps_unknown_code_to_api_fallback() {
        let server = MockServer::start().await;
        mount_catalog(&server).await;

        Mock::given(method("GET"))
            .and(path("/things"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "Something unexpected",
                "code": "ERR_UNKNOWN_999",
                "status": 400
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "key");
        let result: Result<serde_json::Value, _> = client.get("/things").await;

        assert!(
            matches!(result, Err(SynapseError::Api { status: 400, .. })),
            "unknown code must fall back to Api, got: {:?}",
            result
        );
        if let Err(SynapseError::Api { message, .. }) = result {
            assert_eq!(message, "Something unexpected", "should use body message for unknown codes");
        }
    }

    /// Issue an authenticated POST request with a JSON body.
    pub async fn post<T: DeserializeOwned, B: Serialize + Clone + Send + 'static>(
        &self,
        path: &str,
        body: B,
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
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
                    let body = resp.text().await.unwrap_or_default();
                    // Surface structured JSON errors as Api; plain text as Http.
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let message = v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or(&body)
                            .to_string();
                        return Err(SynapseError::Api { status, message });
                    }
                    // Plain-text or non-JSON body: emit as Api with raw body as message.
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.json::<T>().await.map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated GET request with query parameters and deserialize the JSON response.
    ///
    /// The request is retried automatically according to the client's retry
    /// configuration. 4xx responses are returned immediately without retrying.
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        let resp = self.get_response(path).await?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(SynapseError::Http { status, body });
        }
        resp.json::<T>().await.map_err(SynapseError::Network)
    }

    /// Issue an authenticated GET request with query parameters and deserialize JSON.
    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SynapseError> {
        let url = self.build_url(path, query);
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        let query = query.to_vec();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            let query = query.clone();
            async move {
                let resp = http.get(&url).header("X-API-Key", &key).send().await.map_err(SynapseError::Network)?;
                let resp = http
                    .get(&url)
                    .header("X-API-Key", &key)
                    .query(&query)
                    .query(&query)
                    .header("X-API-Key", &key)
                    .send()
                    .await
                    .map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                        let message = v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or(&body)
                            .to_string();
                        return Err(SynapseError::Api { status, message });
                    }
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.json::<T>().await.map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated POST request with a JSON body and deserialize the JSON response.
    ///
    /// 4xx responses are returned immediately without retrying.
    pub async fn post<B: Serialize + Clone, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, SynapseError> {
        let url = format!("{}{}", self.base_url, path);
        let key = self.api_key.clone();
        let http = self.http.clone();
        let body = body.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
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
                    let body_text = resp.text().await.unwrap_or_default();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body_text) {
                        let message = v
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or(&body_text)
                            .to_string();
                        return Err(SynapseError::Api { status, message });
                    }
                    return Err(SynapseError::Api { status, message: body_text });
                    return Err(SynapseError::Api { status, message: body });
                }
                resp.json::<T>().await.map_err(SynapseError::Network)
            }
        })
        .await
    }

    /// Issue an authenticated GET request and deserialize JSON even on non-2xx status.
    pub async fn get_json_with_status<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<(u16, T), SynapseError> {
        let resp = self.get_response(path).await?;
        let status = resp.status().as_u16();
        let body = resp.json::<T>().await.map_err(SynapseError::Network)?;
        Ok((status, body))
    }

    /// Issue an authenticated GET request with query parameters and deserialize JSON even on non-2xx status.
    pub async fn get_query_json_with_status<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<(u16, T), SynapseError> {
        let url = self.build_url(path, query);
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                let resp = http.get(&url).header("X-API-Key", &key).send().await.map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                let body = resp.json::<T>().await.map_err(SynapseError::Network)?;
                Ok((status, body))
            }
        })
        .await
    }

    /// Issue an authenticated GET request and return raw bytes.
    pub async fn get_bytes(&self, path: &str) -> Result<Vec<u8>, SynapseError> {
        let resp = self.get_response(path).await?;
        let status = resp.status().as_u16();
        if status >= 400 {
            let body = resp.text().await.unwrap_or_default();
            return Err(SynapseError::Http { status, body });
        }
        resp.bytes().await.map(|b| b.to_vec()).map_err(SynapseError::Network)
    }

    /// Issue an authenticated GET request with query parameters and return raw bytes.
    pub async fn get_query_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<u8>, SynapseError> {
        let url = self.build_url(path, query);
        let key = self.api_key.clone();
        let http = self.http.clone();
        retry_with_backoff(self.max_attempts, self.base_delay_ms, || {
            let url = url.clone();
            let key = key.clone();
            let http = http.clone();
            async move {
                let resp = http.get(&url).header("X-API-Key", &key).send().await.map_err(SynapseError::Network)?;
                let status = resp.status().as_u16();
                if status >= 400 {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(SynapseError::Http { status, body });
                }
                resp.bytes().await.map(|b| b.to_vec()).map_err(SynapseError::Network)
            }
        })
        .await
    /// Access the transactions resource.
    pub fn transactions(&self) -> crate::resources::transactions::Transactions<'_> {
        crate::resources::transactions::Transactions { client: self }
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

    /// Access the admin resource (requires admin API key).
    pub fn admin(&self) -> crate::resources::admin::Admin<'_> {
        crate::resources::admin::Admin { client: self }
    }
}

impl SynapseClientBuilder {
    /// Set the maximum total number of attempts (default: 3).
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// Disable retry behaviour.
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
            admin_key: self.admin_key,
            max_attempts: self.max_attempts,
            base_delay_ms: self.base_delay_ms,
            catalog: Arc::new(OnceCell::new()),
        }
    }
}
