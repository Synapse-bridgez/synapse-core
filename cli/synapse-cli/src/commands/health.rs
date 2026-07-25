use crate::client::ApiClient;
use crate::formatter::{print_one, OutputFormat};
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};

// Option A: depend on synapse-core so the API handler remains the single source
// of truth. `pub use` preserves the CLI module's existing public type path.
pub use synapse_core::handlers::HealthStatus;

// ── Response types (mirrors src/handlers/mod.rs) ──────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct LivenessResponse {
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReadinessResponse {
    pub status: String,
    pub draining: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorCatalogResponse {
    pub errors: serde_json::Value,
    pub version: String,
}

// ── Subcommand definitions ─────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum HealthCommand {
    /// Check whether the server process is alive (liveness probe).
    ///
    /// Calls GET /live. Always returns HTTP 200 when the process is running.
    /// Does NOT check database or other dependencies — use `health check` for that.
    ///
    /// Example:
    ///   synapse health live
    ///   synapse health live --json
    Live {
        /// Print output as JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },

    /// Check whether the server is ready to accept traffic (readiness probe).
    ///
    /// Calls GET /ready. Returns HTTP 200 when ready, 503 when draining or not yet
    /// initialised. Use this to gate traffic routing decisions.
    ///
    /// Example:
    ///   synapse health ready
    ///   synapse health ready --json
    Ready {
        /// Print output as JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },

    /// Full health check — aggregates dependency status (database, pool, queues).
    ///
    /// Calls GET /health. Returns HTTP 200 when all critical dependencies are healthy,
    /// 503 when the database is unreachable. Includes pool utilisation, pending queue
    /// depth, and WebSocket connection count.
    ///
    /// Example:
    ///   synapse health check
    ///   synapse health check --json
    Check {
        /// Print output as JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },

    /// List all known error codes and their descriptions.
    ///
    /// Calls GET /errors. Returns the complete error catalog used by the API,
    /// useful for interpreting error responses in logs or client applications.
    ///
    /// Edge case: if no error codes are registered the server still returns a
    /// valid response with an empty list — never null.
    ///
    /// Example:
    ///   synapse health errors
    ///   synapse health errors --json
    Errors {
        /// Print output as JSON instead of a human-readable summary
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ─────────────────────────────────────────────────────────────────────

pub async fn run(cmd: HealthCommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        HealthCommand::Live { json } => {
            let resp: LivenessResponse = client.get("/live").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&resp, fmt);
        }
        HealthCommand::Ready { json } => {
            let resp: ReadinessResponse = client.get("/ready").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&resp, fmt);
        }
        HealthCommand::Check { json } => {
            let resp: HealthStatus = client.get("/health").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&resp, fmt);
        }
        HealthCommand::Errors { json } => {
            let resp: ErrorCatalogResponse = client.get("/errors").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&resp, fmt);
        }
    }

    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    // ── health live ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_live_happy_path_table() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/live")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"alive"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: LivenessResponse = client.get("/live").await.unwrap();
        assert_eq!(resp.status, "alive");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn health_live_happy_path_json() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/live")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"alive"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: LivenessResponse = client.get("/live").await.unwrap();
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("alive"));
    }

    #[tokio::test]
    async fn health_live_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/live")
            .with_status(503)
            .with_body("service unavailable")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<LivenessResponse> = client.get("/live").await;
        assert!(result.is_err());
    }

    // ── health ready ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_ready_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/ready")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ready","draining":false}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: ReadinessResponse = client.get("/ready").await.unwrap();
        assert_eq!(resp.status, "ready");
        assert!(!resp.draining);
    }

    #[tokio::test]
    async fn health_ready_draining_returns_503() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/ready")
            .with_status(503)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"not_ready","draining":true}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        // The API client surfaces 503 as Err
        let result: Result<ReadinessResponse> = client.get("/ready").await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("503"));
    }

    #[tokio::test]
    async fn health_ready_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/ready")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"status":"ready","draining":false}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: ReadinessResponse = client.get("/ready").await.unwrap();
        let json = serde_json::to_string_pretty(&resp).unwrap();
        assert!(json.contains("\"status\""));
        assert!(json.contains("ready"));
    }

    // ── health check ───────────────────────────────────────────────────────────

    fn health_body() -> &'static str {
        r#"{
          "status": "healthy",
          "version": "0.1.0",
          "db": "connected",
          "db_pool": {
            "active_connections": 2,
            "idle_connections": 8,
            "max_connections": 10,
            "usage_percent": 20.0
          },
          "pending_queue_depth": 0,
          "current_batch_size": 50,
          "ws_connection_count": 3
        }"#
    }

    #[tokio::test]
    async fn health_check_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(health_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: HealthStatus = client.get("/health").await.unwrap();
        assert_eq!(resp.status, "healthy");
        assert_eq!(resp.db, "connected");
    }

    #[tokio::test]
    async fn health_check_unhealthy_db_returns_503() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/health")
            .with_status(503)
            .with_header("content-type", "application/json")
            .with_body(r#"{
              "status":"unhealthy","version":"0.1.0","db":"disconnected",
              "db_pool":{"active_connections":0,"idle_connections":0,"max_connections":10,"usage_percent":0.0},
              "pending_queue_depth":0,"current_batch_size":0,"ws_connection_count":0
            }"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<HealthStatus> = client.get("/health").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("503"));
    }

    #[tokio::test]
    async fn health_check_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(health_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: HealthStatus = client.get("/health").await.unwrap();
        let json = serde_json::to_string_pretty(&resp).unwrap();
        assert!(json.contains("\"status\""));
        assert!(json.contains("healthy"));
    }

    // ── health errors ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_errors_happy_path() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/errors")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors":{"E001":"not found"},"version":"1.0.0"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: ErrorCatalogResponse = client.get("/errors").await.unwrap();
        assert_eq!(resp.version, "1.0.0");
    }

    /// Edge case: server returns empty error catalog — must be a valid response, not an error.
    #[tokio::test]
    async fn health_errors_empty_catalog_is_valid() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/errors")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors":{},"version":"1.0.0"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: ErrorCatalogResponse = client.get("/errors").await.unwrap();
        assert_eq!(resp.version, "1.0.0");
        // An empty error object is still valid JSON — must not be null/None
        assert!(resp.errors.is_object());
    }

    #[tokio::test]
    async fn health_errors_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/errors")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"errors":{"E001":"not found"},"version":"1.0.0"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: ErrorCatalogResponse = client.get("/errors").await.unwrap();
        let json = serde_json::to_string_pretty(&resp).unwrap();
        assert!(json.contains("\"version\""));
        assert!(json.contains("1.0.0"));
    }
}
