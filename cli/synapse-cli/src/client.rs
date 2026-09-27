use serde::de::DeserializeOwned;
use serde::Serialize;
use synapse_sdk::error::map_status_to_error;
use synapse_sdk::SynapseError;

// ── ApiClient ─────────────────────────────────────────────────────────────────
// Used for tenant-scoped routes authenticated via `X-API-Key` (the
// `TenantContext` extractor on `/transactions*`, `/settlements*`) and for
// the unauthenticated health-probe routes. Never use this for a route
// behind the server's `admin_auth` middleware — see `AdminClient` below.
pub use synapse_sdk::SynapseClient as ApiClient;

// ── AdminClient ──────────────────────────────────────────────────────────────
// HTTP client for every route behind the server's `admin_auth` middleware
// (`src/middleware/auth.rs`): `/admin/*`, `/stats/*`, `/cache/metrics`,
// `/graphql`, and `/export`. `admin_auth` checks `Authorization: Bearer
// <token>` exclusively — it has never accepted `X-API-Key` or `X-Admin-Key`.
//
// This is the single client every admin-class CLI command should go
// through. It used to be duplicated per module (a private copy in
// `commands/admin.rs`, and ad-hoc `ApiClient`/`SynapseCliClient` usage in
// `commands/stats.rs`, `commands/webhooks.rs`, `commands/transactions.rs`,
// `commands/graphql.rs`) — each copy independently guessed a different
// wrong header. Consolidating here means there is exactly one place that
// decides how an admin request authenticates.
pub struct AdminClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AdminClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn with_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            request
        } else {
            request.header("Authorization", format!("Bearer {}", self.api_key))
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        self.send(self.http.get(self.url(path))).await
    }

    pub async fn get_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SynapseError> {
        self.send(self.http.get(self.url(path)).query(query)).await
    }

    /// `GET <base_url><path>?…` returning the raw response bytes (used for
    /// CSV/JSON export downloads).
    pub async fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<u8>, SynapseError> {
        let response = self
            .with_auth(self.http.get(self.url(path)).query(query))
            .send()
            .await
            .map_err(SynapseError::Network)?;
        let status = response.status().as_u16();
        if status >= 400 {
            let body = response.text().await.unwrap_or_default();
            return Err(map_status_to_error(status, extract_error_message(&body), None));
        }
        response
            .bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(SynapseError::Network)
    }

    pub async fn put_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, SynapseError> {
        self.send(self.http.put(self.url(path)).json(&body)).await
    }

    pub async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T, SynapseError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    pub async fn patch_json<T, B>(&self, path: &str, body: &B) -> Result<T, SynapseError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        self.send(self.http.patch(self.url(path)).json(body)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        self.send(self.http.delete(self.url(path))).await
    }

    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, SynapseError> {
        let response = self
            .with_auth(request)
            .send()
            .await
            .map_err(SynapseError::Network)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(SynapseError::Network)?;

        if status >= 400 {
            return Err(map_status_to_error(status, extract_error_message(&body), None));
        }

        serde_json::from_str(&body).map_err(|e| SynapseError::Decode(e.to_string()))
    }
}

/// Extract a human-readable message from an admin API error body (e.g.
/// `{"error": "Bad request: …"}`), falling back to the raw body. Strips the
/// server's `"Bad request: "` prefix so CLI error output stays concise.
fn extract_error_message(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            ["error", "detail", "message"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string());

    message
        .strip_prefix("Bad request: ")
        .unwrap_or(&message)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    /// The server's `admin_auth` middleware checks `Authorization: Bearer
    /// <token>` exclusively (`src/middleware/auth.rs`). `AdminClient` must
    /// send exactly that header — this is the regression test for the
    /// X-API-Key/X-Admin-Key bugs this client replaced.
    #[tokio::test]
    async fn sends_authorization_bearer_header_with_correct_token() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/admin/locks")
            .match_header("authorization", "Bearer correct-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "correct-token");
        let result: Result<serde_json::Value, SynapseError> = client.get("/admin/locks").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        mock.assert_async().await;
    }

    /// A request with no credential configured must never send an
    /// `X-API-Key` fallback (that was the root cause of the bug this client
    /// fixes) — it should simply omit the `Authorization` header.
    #[tokio::test]
    async fn never_sends_x_api_key_or_x_admin_key() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/admin/locks")
            .match_header("x-api-key", mockito::Matcher::Missing)
            .match_header("x-admin-key", mockito::Matcher::Missing)
            .match_header("authorization", "Bearer correct-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "correct-token");
        let _: serde_json::Value = client.get("/admin/locks").await.unwrap();
        mock.assert_async().await;
    }

    /// A missing or incorrect token must surface as a typed `Unauthorized`
    /// error (so `main.rs` can map it to `EXIT_AUTH_FAILURE`), not just a
    /// generic failure.
    #[tokio::test]
    async fn wrong_token_returns_unauthorized_error() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/admin/locks")
            .match_header("authorization", "Bearer wrong-token")
            .with_status(401)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"invalid admin credentials"}"#)
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "wrong-token");
        let result: Result<serde_json::Value, SynapseError> = client.get("/admin/locks").await;

        assert!(matches!(result, Err(SynapseError::Unauthorized(_))));
    }

    #[test]
    fn extracts_message_and_strips_bad_request_prefix() {
        assert_eq!(
            extract_error_message(r#"{"error":"Bad request: limit must be positive"}"#),
            "limit must be positive"
        );
        assert_eq!(extract_error_message("not json"), "not json");
    }
}
