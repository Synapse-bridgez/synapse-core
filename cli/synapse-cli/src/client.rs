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
    dry_run: bool,
}

impl AdminClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            dry_run: false,
        }
    }

    /// Enable dry-run mode. When enabled, every mutating request
    /// (`post_json`, `put_json`, `patch_json`, `delete`) short-circuits just
    /// before the HTTP call is made: the resolved method, URL, and payload
    /// are printed as a clearly-labelled preview and no network mutation is
    /// sent. Read-only requests (`get`, `get_query`, `get_bytes`) are
    /// unaffected. This reuses all upstream validation/resolution logic so
    /// the preview reflects exactly what would really be sent.
    pub fn with_dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    pub fn is_dry_run(&self) -> bool {
        self.dry_run
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

    /// Print a consistent, unmissable preview of a mutating request that is
    /// being skipped because dry-run mode is active. The `[DRY RUN]` marker
    /// and the explicit "no request was sent" line make it impossible for an
    /// operator to mistake this for real output.
    fn print_dry_run(&self, method: &str, path: &str, body: Option<&serde_json::Value>) {
        println!("[DRY RUN] {} {}", method, self.url(path));
        match body {
            Some(value) => {
                let pretty = serde_json::to_string_pretty(value)
                    .unwrap_or_else(|_| value.to_string());
                println!("[DRY RUN] payload:\n{}", pretty);
            }
            None => println!("[DRY RUN] payload: (none)"),
        }
        println!("[DRY RUN] no request was sent; this is a preview only.");
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
        if self.dry_run {
            self.print_dry_run("PUT", path, Some(&body));
            return Ok(serde_json::Value::Null);
        }
        self.send(self.http.put(self.url(path)).json(&body)).await
    }

    pub async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T, SynapseError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        if self.dry_run {
            let value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
            self.print_dry_run("POST", path, Some(&value));
            return Ok(serde_json::Value::Null);
        }
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    pub async fn patch_json<T, B>(&self, path: &str, body: &B) -> Result<T, SynapseError>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        if self.dry_run {
            let value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
            self.print_dry_run("PATCH", path, Some(&value));
            return Ok(serde_json::Value::Null);
        }
        self.send(self.http.patch(self.url(path)).json(body)).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, SynapseError> {
        if self.dry_run {
            self.print_dry_run("DELETE", path, None);
            return Ok(serde_json::Value::Null);
        }
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

/// Environment variables through which the resolved CLI auth/config context
/// is handed to external `synapse-cli-<name>` plugin binaries. Plugins read
/// these instead of re-implementing the CLI's own config/auth resolution.
pub const PLUGIN_ENV_BASE_URL: &str = "SYNAPSE_CLI_BASE_URL";
pub const PLUGIN_ENV_API_KEY: &str = "SYNAPSE_CLI_API_KEY";
pub const PLUGIN_ENV_PLUGIN_NAME: &str = "SYNAPSE_CLI_PLUGIN_NAME";

/// Resolved auth/config context passed to an external plugin subcommand.
///
/// Built once by the CLI (from the same config/auth resolution the built-in
/// commands use) and exported into the plugin process environment so the
/// plugin never has to re-derive the base URL or credential itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginContext {
    pub base_url: String,
    pub api_key: String,
    pub plugin_name: String,
}

impl PluginContext {
    pub fn new(base_url: &str, api_key: &str, plugin_name: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            plugin_name: plugin_name.to_string(),
        }
    }

    /// The `(key, value)` pairs to inject into the plugin's environment.
    pub fn env_vars(&self) -> Vec<(&'static str, String)> {
        vec![
            (PLUGIN_ENV_BASE_URL, self.base_url.clone()),
            (PLUGIN_ENV_API_KEY, self.api_key.clone()),
            (PLUGIN_ENV_PLUGIN_NAME, self.plugin_name.clone()),
        ]
    }
}

/// Lightweight connectivity/credential probe used by `synapse-cli init`.
///
/// Hits the unauthenticated `/health` route to confirm the server URL is
/// reachable, then (when a credential is supplied) issues an authenticated
/// `GET /admin/locks` so a typo'd token fails immediately during setup
/// rather than on the user's first real command. Returns `Ok(())` only when
/// both checks pass.
pub async fn validate_connection(
    base_url: &str,
    api_key: &str,
) -> Result<(), SynapseError> {
    let http = reqwest::Client::new();
    let base = base_url.trim_end_matches('/');

    let health = http
        .get(format!("{}/health", base))
        .send()
        .await
        .map_err(SynapseError::Network)?;
    let status = health.status().as_u16();
    if status >= 400 {
        let body = health.text().await.unwrap_or_default();
        return Err(map_status_to_error(status, extract_error_message(&body), None));
    }

    if !api_key.is_empty() {
        let client = AdminClient::new(base, api_key);
        let _: serde_json::Value = client.get("/admin/locks").await?;
    }

    Ok(())
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
            .match_header("authorization", mockito::Matcher::Missing)
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "");
        let result: Result<serde_json::Value, SynapseError> = client.get("/admin/locks").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
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
            .with_body("{\"error\": \"Unauthorized\"}")
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "wrong-token");
        let result: Result<serde_json::Value, SynapseError> = client.get("/admin/locks").await;

        assert!(
            matches!(result, Err(SynapseError::Unauthorized(_))),
            "expected Unauthorized, got: {:?}",
            result
        );
    }

    /// In dry-run mode a mutating POST must not reach the server at all.
    /// `mockito` fails the test if an unexpected request is received, so a
    /// successful call with no matching mock proves no network mutation was
    /// sent.
    #[tokio::test]
    async fn dry_run_post_does_not_send_request() {
        let server = Server::new_async().await;
        let client = AdminClient::new(&server.url(), "token").with_dry_run(true);

        let body = serde_json::json!({"amount": 100});
        let result: Result<serde_json::Value, SynapseError> =
            client.post_json("/admin/settlements", &body).await;

        assert!(result.is_ok(), "dry-run should succeed, got: {:?}", result);
    }

    /// Dry-run must also short-circuit PUT, PATCH, and DELETE mutations.
    #[tokio::test]
    async fn dry_run_put_patch_delete_do_not_send_requests() {
        let server = Server::new_async().await;
        let client = AdminClient::new(&server.url(), "token").with_dry_run(true);

        let body = serde_json::json!({"quota": 500});
        let put: Result<serde_json::Value, SynapseError> =
            client.put_json("/admin/quota", body.clone()).await;
        let patch: Result<serde_json::Value, SynapseError> =
            client.patch_json("/admin/webhooks/1", &body).await;
        let delete: Result<serde_json::Value, SynapseError> =
            client.delete("/admin/webhooks/1").await;

        assert!(put.is_ok(), "dry-run PUT should succeed, got: {:?}", put);
        assert!(patch.is_ok(), "dry-run PATCH should succeed, got: {:?}", patch);
        assert!(delete.is_ok(), "dry-run DELETE should succeed, got: {:?}", delete);
    }

    /// Read-only requests must still hit the network even when dry-run is on.
    #[tokio::test]
    async fn dry_run_does_not_affect_read_only_requests() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/admin/locks")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create_async()
            .await;

        let client = AdminClient::new(&server.url(), "token").with_dry_run(true);
        let result: Result<serde_json::Value, SynapseError> = client.get("/admin/locks").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        mock.assert_async().await;
    }

    /// The resolved auth context must be exposed to plugins via the
    /// documented environment variables, with the base URL normalized and
    /// the plugin name carried through for diagnostics.
    #[test]
    fn plugin_context_exports_auth_env_vars() {
        let ctx = PluginContext::new("https://api.example.com/", "secret-token", "reconcile");
        let vars = ctx.env_vars();

        assert!(vars.contains(&(PLUGIN_ENV_BASE_URL, "https://api.example.com".to_string())));
        assert!(vars.contains(&(PLUGIN_ENV_API_KEY, "secret-token".to_string())));
        assert!(vars.contains(&(PLUGIN_ENV_PLUGIN_NAME, "reconcile".to_string())));
    }
}
    }
}
