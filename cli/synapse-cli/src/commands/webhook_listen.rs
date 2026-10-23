//! `synapse webhooks listen`
//!
//! Local webhook-testing proxy, in the spirit of `stripe listen`:
//!
//! 1. Opens a public tunnel to a local port using an existing tunnelling tool
//!    (`cloudflared` quick tunnels by default, or any tool via `--tunnel-cmd`,
//!    or an already-running tunnel via `--tunnel-url`).
//! 2. Registers the tunnel URL as a temporary webhook endpoint through the
//!    normal admin batch API (`POST /admin/webhooks/endpoints/batch`).
//! 3. Waits for Ctrl-C / SIGTERM, then disables the temporary endpoint.
//!
//! The tunnel hands deliveries straight to the local port, so the original
//! `X-Webhook-Timestamp` / `X-Webhook-Signature` headers and the raw body reach
//! the local handler untouched and can be verified with the printed secret.
//!
//! Cleanup runs on normal exit, Ctrl-C (SIGINT), SIGTERM, and when the tunnel
//! process dies on its own. SIGKILL cannot be intercepted; if the CLI is
//! killed that way the endpoint stays registered and its ID is printed at
//! startup so it can be disabled manually.

use crate::client::AdminClient;
use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::json;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

const BATCH_PATH: &str = "/admin/webhooks/endpoints/batch";

#[derive(Args)]
pub struct WebhooksCmd {
    #[command(subcommand)]
    pub command: WebhooksSubcommand,
}

#[derive(Subcommand)]
pub enum WebhooksSubcommand {
    /// Forward real webhook deliveries to a local port through a tunnel.
    ///
    /// Registers a temporary webhook endpoint, prints its signing secret, and
    /// removes the endpoint again when the command exits.
    ///
    /// Example:
    ///   synapse webhooks listen --port 4000 --path /hooks/synapse
    Listen {
        /// Local port the webhook handler is listening on.
        #[arg(long)]
        port: u16,

        /// Path on the local handler that receives deliveries.
        #[arg(long, default_value = "/")]
        path: String,

        /// Signing secret for the temporary endpoint. Generated when omitted.
        #[arg(long, env = "SYNAPSE_WEBHOOK_SECRET")]
        secret: Option<String>,

        /// Use an already-running tunnel's public URL instead of starting one.
        #[arg(long)]
        tunnel_url: Option<String>,

        /// Tunnel command to run; `{port}` is replaced with the local port.
        /// The first https:// URL it prints is used as the public URL.
        #[arg(
            long,
            default_value = "cloudflared tunnel --no-autoupdate --url http://localhost:{port}"
        )]
        tunnel_cmd: String,
    },
}

#[derive(Deserialize)]
struct BatchResponse {
    results: Vec<BatchResult>,
}

#[derive(Deserialize)]
struct BatchResult {
    success: bool,
    endpoint_id: Option<String>,
    error: Option<String>,
}

/// Join the public tunnel URL with the local handler path.
pub fn build_public_url(tunnel_url: &str, path: &str) -> String {
    let base = tunnel_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    }
}

/// Extract the first `https://` URL from a line of tunnel output.
pub fn extract_tunnel_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let url: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '|' | '"' | '\'' | ')'))
        .collect();
    (url.len() > "https://".len()).then_some(url)
}

fn generate_secret() -> String {
    format!(
        "whsec_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

async fn start_tunnel(cmd_template: &str, port: u16) -> Result<(Child, String)> {
    let cmdline = cmd_template.replace("{port}", &port.to_string());
    let mut parts = cmdline.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| anyhow!("--tunnel-cmd is empty"))?;

    let mut child = Command::new(program)
        .args(parts)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            anyhow!("failed to start tunnel '{program}': {e}. Install it or pass --tunnel-url")
        })?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(line);
            }
        });
    }

    let wait = async {
        while let Some(line) = rx.recv().await {
            if let Some(url) = extract_tunnel_url(&line) {
                return Some(url);
            }
        }
        None
    };
    match tokio::time::timeout(std::time::Duration::from_secs(30), wait).await {
        Ok(Some(url)) => Ok((child, url)),
        Ok(None) => bail!("tunnel exited before printing a public URL"),
        Err(_) => bail!("timed out waiting for the tunnel to print a public URL"),
    }
}

async fn register(client: &AdminClient, url: &str, secret: &str) -> Result<String> {
    let body = json!([{ "operation": "create", "create": { "url": url, "secret": secret } }]);
    let resp: BatchResponse = client.post_json(BATCH_PATH, &body).await?;
    let result = resp
        .results
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("empty response registering webhook endpoint"))?;
    if !result.success {
        bail!(
            "failed to register webhook endpoint: {}",
            result.error.unwrap_or_default()
        );
    }
    result
        .endpoint_id
        .ok_or_else(|| anyhow!("server did not return an endpoint id"))
}

async fn unregister(client: &AdminClient, id: &str) -> Result<()> {
    let body = json!([{ "operation": "disable", "disable": { "id": id } }]);
    let _: serde_json::Value = client.post_json(BATCH_PATH, &body).await?;
    Ok(())
}

/// Resolve when the process is asked to stop (Ctrl-C or SIGTERM).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn run(cmd: WebhooksSubcommand, base_url: &str, api_key: &str) -> Result<()> {
    match cmd {
        WebhooksSubcommand::Listen {
            port,
            path,
            secret,
            tunnel_url,
            tunnel_cmd,
        } => {
            let client = AdminClient::new(base_url, api_key);
            let secret = secret.unwrap_or_else(generate_secret);

            let (mut tunnel, public) = match tunnel_url {
                Some(url) => (None, url),
                None => {
                    let (child, url) = start_tunnel(&tunnel_cmd, port).await?;
                    (Some(child), url)
                }
            };
            let endpoint_url = build_public_url(&public, &path);

            let id = register(&client, &endpoint_url, &secret).await?;
            eprintln!("Forwarding {endpoint_url} -> http://localhost:{port}{path}");
            eprintln!("Temporary webhook endpoint: {id}");
            eprintln!("Signing secret: {secret}");
            eprintln!("Press Ctrl-C to stop and remove the endpoint.");

            // Exit on a signal, or if the tunnel dies underneath us.
            let tunnel_exit = async {
                match tunnel.as_mut() {
                    Some(child) => {
                        let _ = child.wait().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = shutdown_signal() => eprintln!("\nShutting down..."),
                _ = tunnel_exit => eprintln!("Tunnel exited; shutting down..."),
            }

            let cleanup = unregister(&client, &id).await;
            if let Some(child) = tunnel.as_mut() {
                let _ = child.kill().await;
            }
            match cleanup {
                Ok(()) => eprintln!("Removed temporary webhook endpoint {id}"),
                Err(e) => bail!("failed to remove temporary webhook endpoint {id}: {e}"),
            }
            Ok(())
        }
    }
}
