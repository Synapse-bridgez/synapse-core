//! `synapse admin webhooks health` and `synapse admin webhooks health-get <id>`
//!
//! Fetches webhook endpoint health snapshots from the Synapse admin API:
//!
//! - `health`        → `GET /admin/webhooks/health`   (list all endpoints)
//! - `health-get`    → `GET /admin/webhooks/health/:id` (single endpoint by UUID)
//!
//! Both subcommands print via the shared table/JSON formatter.
//!
//! # 404 edge case
//! `health-get` with an unknown endpoint ID returns HTTP 404.  This is
//! explicitly distinguished from a transport error: the CLI prints a
//! human-readable "endpoint not found" message and exits 1, rather than
//! emitting a raw network-error stack trace.

use crate::client::ApiClient;
use crate::formatter::{print, OutputFormat, TableDisplay};
use anyhow::{bail, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};

// ── Response types ─────────────────────────────────────────────────────────────
// Mirror `EndpointHealth` in `src/services/webhook_dispatcher.rs` and
// `EndpointHealth` in `sdks/rust/src/admin_models.rs`.

/// Health snapshot for a single webhook endpoint.
///
/// Mirrors the server's `EndpointHealth` struct
/// (`src/services/webhook_dispatcher.rs`).  `success_rate` is a fraction in
/// `[0.0, 1.0]`, not a percentage.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EndpointHealth {
    /// UUID of the webhook endpoint.
    pub id: String,
    /// Target URL the endpoint delivers to.
    pub url: String,
    /// Whether the endpoint is currently active.
    pub enabled: bool,
    /// Delivery success rate in `[0.0, 1.0]`.
    pub success_rate: f64,
    /// Cumulative number of delivery attempts.
    pub total_deliveries: i32,
    /// ISO 8601 timestamp of the last successful delivery, if any.
    pub last_success_at: Option<String>,
}

// ── JSON display view ─────────────────────────────────────────────────────────
// `EndpointHealth::success_rate` is kept as a `[0.0, 1.0]` fraction internally
// (matching the wire format and the table row's `* 100.0` formatting below),
// but JSON output presents it as a percentage for consistency with the table.

#[derive(Serialize)]
struct EndpointHealthJson<'a> {
    id: &'a str,
    url: &'a str,
    enabled: bool,
    success_rate: f64,
    total_deliveries: i32,
    last_success_at: Option<&'a str>,
}

impl<'a> From<&'a EndpointHealth> for EndpointHealthJson<'a> {
    fn from(e: &'a EndpointHealth) -> Self {
        Self {
            id: &e.id,
            url: &e.url,
            enabled: e.enabled,
            success_rate: e.success_rate * 100.0,
            total_deliveries: e.total_deliveries,
            last_success_at: e.last_success_at.as_deref(),
        }
    }
}

// ── TableDisplay ───────────────────────────────────────────────────────────────

impl TableDisplay for EndpointHealth {
    fn headers() -> Vec<&'static str> {
        vec![
            "ID",
            "URL",
            "ENABLED",
            "SUCCESS RATE",
            "TOTAL DELIVERIES",
            "LAST SUCCESS",
        ]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.url.clone(),
            self.enabled.to_string(),
            format!("{:.1}%", self.success_rate * 100.0),
            self.total_deliveries.to_string(),
            self.last_success_at
                .as_deref()
                .unwrap_or("-")
                .to_string(),
        ]
    }
}

// ── Subcommand definitions ─────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum WebhooksCommand {
    /// List health snapshots for all webhook endpoints.
    ///
    /// Calls GET /admin/webhooks/health and prints a table (or JSON array)
    /// with the `id`, `url`, `enabled` flag, `success_rate`, `total_deliveries`,
    /// and `last_success_at` for every registered endpoint.
    ///
    /// Edge case: if no endpoints are registered the server returns an empty
    /// array — never null.  The CLI prints "(no results)" in table mode.
    ///
    /// Exit codes:
    ///   0 – success (including empty list)
    ///   1 – network or server error
    ///
    /// Example:
    ///   synapse admin webhooks health
    ///   synapse admin webhooks health --json
    #[command(
        name = "health",
        about = "List health snapshots for all webhook endpoints",
        long_about = "Calls GET /admin/webhooks/health.\n\n\
                      Prints a table with id, url, enabled, success_rate, \
                      total_deliveries, and last_success_at.\n\n\
                      Edge case: an empty endpoint list is valid (exits 0).\n\n\
                      Exit codes:\n  0 - Success\n  1 - Server or network error"
    )]
    Health {
        /// Print output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Get the health snapshot for a single webhook endpoint by UUID.
    ///
    /// Calls GET /admin/webhooks/health/:id and prints the endpoint's health
    /// fields as a key-value table (or JSON object).
    ///
    /// Edge case: if the endpoint ID does not exist the server returns HTTP 404.
    /// This is reported as a clear "endpoint not found: <id>" message rather
    /// than a generic transport error, and the process exits 1.
    ///
    /// Exit codes:
    ///   0 – found and printed
    ///   1 – endpoint not found (404) OR network/server error
    ///
    /// Example:
    ///   synapse admin webhooks health-get 550e8400-e29b-41d4-a716-446655440000
    ///   synapse admin webhooks health-get 550e8400-e29b-41d4-a716-446655440000 --json
    #[command(
        name = "health-get",
        about = "Get the health snapshot for a single webhook endpoint by UUID",
        long_about = "Calls GET /admin/webhooks/health/:id.\n\n\
                      Edge case: HTTP 404 is reported as \"endpoint not found: <id>\" \
                      (exit 1), not as a generic transport error.\n\n\
                      Exit codes:\n  0 - Found\n  1 - Not found or server/network error"
    )]
    HealthGet {
        /// UUID of the webhook endpoint to fetch.
        #[arg(value_name = "ID")]
        id: String,

        /// Print output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ─────────────────────────────────────────────────────────────────────

pub async fn run(cmd: WebhooksCommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        // ── synapse admin webhooks health ─────────────────────────────────
        WebhooksCommand::Health { json } => {
            let endpoints: Vec<EndpointHealth> =
                client.get("/admin/webhooks/health").await?;
            if json {
                let views: Vec<EndpointHealthJson> =
                    endpoints.iter().map(EndpointHealthJson::from).collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&views).unwrap_or_else(|_| "[]".into())
                );
            } else {
                print(&endpoints, OutputFormat::Table);
            }
        }

        // ── synapse admin webhooks health-get <ID> ────────────────────────
        //
        // Edge case: a 404 must be surfaced as "endpoint not found: <id>"
        // rather than an opaque "server returned 404: …" transport error.
        WebhooksCommand::HealthGet { id, json } => {
            let path = format!("/admin/webhooks/health/{}", id);
            // Use the raw_get helper so we can inspect the status code before
            // committing to a decode path.
            match client.get_raw_status(&path).await {
                Ok((404, _body)) => {
                    bail!("endpoint not found: {}", id);
                }
                Ok((status, body)) if status >= 400 => {
                    bail!("server returned {}: {}", status, body);
                }
                Ok((_status, body)) => {
                    let endpoint: EndpointHealth = serde_json::from_str(&body)
                        .map_err(|e| anyhow::anyhow!("failed to decode response: {}", e))?;
                    if json {
                        let view = EndpointHealthJson::from(&endpoint);
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&view).unwrap_or_else(|_| "{}".into())
                        );
                    } else {
                        println!("id: {}", endpoint.id);
                        println!("url: {}", endpoint.url);
                        println!("enabled: {}", endpoint.enabled);
                        println!("success_rate: {:.1}%", endpoint.success_rate * 100.0);
                        println!("total_deliveries: {}", endpoint.total_deliveries);
                        println!(
                            "last_success_at: {}",
                            endpoint.last_success_at.as_deref().unwrap_or("-")
                        );
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    Ok(())
}

// ── Unit tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    // ── helper bodies ─────────────────────────────────────────────────────────

    fn endpoint_body(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "url": "https://example.com/webhook",
                "enabled": true,
                "success_rate": 0.98,
                "total_deliveries": 250,
                "last_success_at": "2026-06-30T03:00:00Z"
            }}"#
        )
    }

    fn endpoint_list_body() -> String {
        format!(
            r#"[{}, {}]"#,
            endpoint_body("aaaaaaaa-0000-0000-0000-000000000001"),
            r#"{
                "id": "aaaaaaaa-0000-0000-0000-000000000002",
                "url": "https://other.example.com/hook",
                "enabled": false,
                "success_rate": 0.50,
                "total_deliveries": 40,
                "last_success_at": null
            }"#
        )
    }

    // ── health: list all endpoints ────────────────────────────────────────────

    #[tokio::test]
    async fn health_list_happy_path_deserialises() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/admin/webhooks/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(endpoint_list_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Vec<EndpointHealth>> =
            client.get("/admin/webhooks/health").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let endpoints = result.unwrap();
        assert_eq!(endpoints.len(), 2);
        assert_eq!(endpoints[0].id, "aaaaaaaa-0000-0000-0000-000000000001");
        assert!((endpoints[0].success_rate - 0.98).abs() < 1e-6);
        assert_eq!(endpoints[0].total_deliveries, 250);
        assert!(endpoints[0].enabled);
        assert!(!endpoints[1].enabled);
        assert_eq!(endpoints[1].last_success_at, None);
    }

    /// Edge case: empty endpoint list must not be an error.
    #[tokio::test]
    async fn health_list_empty_is_not_an_error() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/admin/webhooks/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Vec<EndpointHealth>> =
            client.get("/admin/webhooks/health").await;

        assert!(
            result.is_ok(),
            "empty list must be Ok, not an error: {:?}",
            result
        );
        assert!(
            result.unwrap().is_empty(),
            "empty list must be an empty Vec, not null"
        );
    }

    #[tokio::test]
    async fn health_list_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/admin/webhooks/health")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Vec<EndpointHealth>> =
            client.get("/admin/webhooks/health").await;

        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("500"),
            "error message should mention status 500"
        );
    }

    // ── health-get: single endpoint ───────────────────────────────────────────

    #[tokio::test]
    async fn health_get_happy_path_deserialises() {
        let id = "aaaaaaaa-0000-0000-0000-000000000001";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/admin/webhooks/health/{id}").as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(endpoint_body(id))
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<EndpointHealth> =
            client.get(&format!("/admin/webhooks/health/{id}")).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let ep = result.unwrap();
        assert_eq!(ep.id, id);
        assert_eq!(ep.url, "https://example.com/webhook");
        assert!(ep.enabled);
        assert_eq!(ep.total_deliveries, 250);
        assert_eq!(
            ep.last_success_at.as_deref(),
            Some("2026-06-30T03:00:00Z")
        );
    }

    /// Edge case: HTTP 404 must be distinguishable from a transport error.
    /// The runner converts a 404 to "endpoint not found: <id>", not a raw
    /// "server returned 404" message.
    #[tokio::test]
    async fn health_get_404_returns_not_found_error() {
        let id = "00000000-0000-0000-0000-000000000000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/admin/webhooks/health/{id}").as_str())
            .with_status(404)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"endpoint not found"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        // Confirm the raw client surfaces 404 as an error.
        let result: Result<EndpointHealth> =
            client.get(&format!("/admin/webhooks/health/{id}")).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("404"),
            "error message should mention 404, got: {err}"
        );
    }

    #[tokio::test]
    async fn health_get_server_error_returns_err() {
        let id = "aaaaaaaa-0000-0000-0000-000000000001";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/admin/webhooks/health/{id}").as_str())
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<EndpointHealth> =
            client.get(&format!("/admin/webhooks/health/{id}")).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    // ── TableDisplay ─────────────────────────────────────────────────────────

    #[test]
    fn table_display_headers() {
        let headers = EndpointHealth::headers();
        assert!(headers.contains(&"ID"));
        assert!(headers.contains(&"URL"));
        assert!(headers.contains(&"ENABLED"));
        assert!(headers.contains(&"SUCCESS RATE"));
        assert!(headers.contains(&"TOTAL DELIVERIES"));
        assert!(headers.contains(&"LAST SUCCESS"));
    }

    #[test]
    fn table_display_row_with_last_success() {
        let ep = EndpointHealth {
            id: "abc-123".to_string(),
            url: "https://example.com/hook".to_string(),
            enabled: true,
            success_rate: 0.95,
            total_deliveries: 100,
            last_success_at: Some("2026-06-30T01:00:00Z".to_string()),
        };
        let row = ep.row();
        assert_eq!(row[0], "abc-123");
        assert_eq!(row[1], "https://example.com/hook");
        assert_eq!(row[2], "true");
        assert_eq!(row[3], "95.0%");
        assert_eq!(row[4], "100");
        assert_eq!(row[5], "2026-06-30T01:00:00Z");
    }

    #[test]
    fn table_display_row_without_last_success() {
        let ep = EndpointHealth {
            id: "xyz".to_string(),
            url: "https://hook.example.com/".to_string(),
            enabled: false,
            success_rate: 0.0,
            total_deliveries: 0,
            last_success_at: None,
        };
        let row = ep.row();
        assert_eq!(row[2], "false");
        assert_eq!(row[3], "0.0%");
        assert_eq!(row[5], "-");
    }

    #[test]
    fn table_display_row_full_success_rate() {
        let ep = EndpointHealth {
            id: "id".to_string(),
            url: "https://u.test/".to_string(),
            enabled: true,
            success_rate: 1.0,
            total_deliveries: 500,
            last_success_at: None,
        };
        let row = ep.row();
        assert_eq!(row[3], "100.0%");
        assert_eq!(row[4], "500");
    }
}
