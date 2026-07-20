//! `synapse admin ...` — administrative operations exposed by the Synapse API:
//! reconciliation, settlement status overrides, distributed lock inspection,
//! tenant quota management, bulk transaction status updates, webhook health,
//! and the event stream (watch / reconnect / reconnect-status).

use crate::commands::webhooks;
use crate::formatter::{Formatter, OutputFormat};
use crate::output;
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Admin-only HTTP client ──────────────────────────────────────────────────────
// The admin API surfaces richer error bodies (e.g. `{"error": "Bad request: …"}`)
// than the plain `crate::client::ApiClient`, and needs PUT/PATCH/DELETE — so it
// gets its own small client rather than overloading the shared one.

struct AdminClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl AdminClient {
    fn new(base_url: &str, api_key: &str) -> Self {
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
            request.header("X-API-Key", &self.api_key)
        }
    }

    async fn get<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.send(self.http.get(self.url(path))).await
    }

    async fn put_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T> {
        self.send(self.http.put(self.url(path)).json(&body)).await
    }

    async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    async fn patch_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        self.send(self.http.patch(self.url(path)).json(body)).await
    }

    async fn delete<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        self.send(self.http.delete(self.url(path))).await
    }

    async fn get_bytes(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let response = self
            .with_auth(self.http.get(self.url(path)).query(query))
            .send()
            .await
            .context("request failed")?;

        let status = response.status();
        let body = response.bytes().await.context("failed to read response body")?;
        if !status.is_success() {
            bail!("server returned {status}: {}", String::from_utf8_lossy(&body));
        }

        Ok(body.to_vec())
    }

    async fn send<T: for<'de> Deserialize<'de>>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        let response = self.with_auth(request).send().await.context("request failed")?;
        let status = response.status();
        let body = response.text().await.context("failed to read response body")?;

        if !status.is_success() {
            bail!("{}", server_error_message(&body));
        }

        serde_json::from_str(&body).context("failed to parse response JSON")
    }
}

fn server_error_message(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            ["error", "detail", "message"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string());

    message.strip_prefix("Bad request: ").unwrap_or(&message).to_string()
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

// ── Top-level admin command tree ────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum AdminCommands {
    /// Reconciliation reports and runs.
    #[command(subcommand)]
    Reconciliation(ReconciliationCommands),

    /// Settlement administration.
    #[command(subcommand)]
    Settlements(AdminSettlementCommands),

    /// Distributed lock administration.
    #[command(subcommand)]
    Locks(LockCommands),

    /// Tenant quota administration.
    #[command(subcommand)]
    Quotas(QuotaCommands),

    /// Bulk transaction administration.
    #[command(subcommand)]
    Transactions(TransactionAdminCommands),

    /// Webhook endpoint health.
    #[command(subcommand)]
    Webhooks(webhooks::WebhooksCommand),

    /// Event stream commands.
    #[command(subcommand)]
    Events(AdminEventsCommands),
}

pub async fn run(cmd: AdminCommands, base_url: &str, api_key: &str) -> Result<()> {
    match cmd {
        AdminCommands::Reconciliation(command) => handle_reconciliation(base_url, api_key, command).await,
        AdminCommands::Settlements(command) => handle_admin_settlements(base_url, api_key, command).await,
        AdminCommands::Locks(command) => handle_locks(base_url, api_key, command).await,
        AdminCommands::Quotas(command) => handle_quotas(base_url, api_key, command).await,
        AdminCommands::Transactions(command) => handle_transactions(base_url, api_key, command).await,
        AdminCommands::Webhooks(command) => webhooks::run(command, base_url, api_key).await,
        AdminCommands::Events(command) => handle_events(base_url, api_key, command).await,
    }
}

// ── Reconciliation ────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum ReconciliationCommands {
    #[command(
        about = "List reconciliation reports",
        long_about = "List reconciliation reports generated by the admin API.\n\nRequired flags: none.\nOptional flags:\n  --limit <LIMIT>   Maximum number of reports to return (default: 20).\n  --offset <OFFSET>  Number of reports to skip before returning results (default: 0).\n  --json            Print the raw API response as JSON."
    )]
    Reports {
        #[arg(long, value_name = "LIMIT", default_value_t = 20)]
        limit: u32,

        #[arg(long, value_name = "OFFSET", default_value_t = 0)]
        offset: u32,

        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Show a reconciliation report",
        long_about = "Fetch one reconciliation report by UUID and print the full response body.\n\nRequired flags:\n  <REPORT_ID>       UUID of the report to fetch.\nOptional flags:\n  --json            Print the raw API response as JSON."
    )]
    Report {
        #[arg(value_name = "REPORT_ID")]
        report_id: Uuid,

        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Run a reconciliation report",
        long_about = "Run a reconciliation pass for one Stellar account and persist the result.\n\nRequired flags:\n  --account <ACCOUNT>      Stellar account to reconcile.\nOptional flags:\n  --period-hours <HOURS>   Hours of history to include (default: 24).\n  --json                   Print the raw API response as JSON."
    )]
    Run {
        #[arg(long, value_name = "ACCOUNT")]
        account: String,

        #[arg(long, value_name = "HOURS", default_value_t = 24)]
        period_hours: u32,

        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct ListReportsResponse {
    reports: Vec<ReportSummary>,
    total: i64,
    limit: i32,
    offset: i32,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportSummary {
    id: Uuid,
    generated_at: String,
    period_start: String,
    period_end: String,
    total_db_transactions: i32,
    total_chain_payments: i32,
    missing_on_chain_count: i32,
    orphaned_payments_count: i32,
    amount_mismatches_count: i32,
    has_discrepancies: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportDetailResponse {
    id: Uuid,
    generated_at: String,
    period_start: String,
    period_end: String,
    summary: ReportDetailSummary,
    missing_on_chain: Vec<serde_json::Value>,
    orphaned_payments: Vec<serde_json::Value>,
    amount_mismatches: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportDetailSummary {
    total_db_transactions: usize,
    total_chain_payments: usize,
    missing_on_chain_count: i32,
    orphaned_payments_count: i32,
    amount_mismatches_count: i32,
    has_discrepancies: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RunResponse {
    message: String,
    report: ReportSummary,
}

#[derive(Debug, Serialize)]
struct RunRequest<'a> {
    account: &'a str,
    period_hours: Option<u32>,
}

async fn handle_reconciliation(base_url: &str, api_key: &str, command: ReconciliationCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        ReconciliationCommands::Reports { limit, offset, json } => {
            let response: ListReportsResponse = client
                .get(&format!(
                    "/admin/reconciliation/reports?limit={limit}&offset={offset}"
                ))
                .await?;
            println!("{}", output::render(&response, json, format_reports_table)?);
        }
        ReconciliationCommands::Report { report_id, json } => {
            let response: ReportDetailResponse = client
                .get(&format!("/admin/reconciliation/reports/{report_id}"))
                .await?;
            println!("{}", output::render(&response, json, format_report_table)?);
        }
        ReconciliationCommands::Run {
            account,
            period_hours,
            json,
        } => {
            let response: RunResponse = client
                .post_json(
                    "/admin/reconciliation/run",
                    &RunRequest {
                        account: &account,
                        period_hours: Some(period_hours),
                    },
                )
                .await?;
            println!("{}", output::render(&response, json, format_run_table)?);
        }
    }

    Ok(())
}

fn format_reports_table(response: &ListReportsResponse) -> String {
    let mut lines = vec![format!(
        "Reports: {} total (showing {} from offset {}, limit {})",
        response.total,
        response.reports.len(),
        response.offset,
        response.limit
    )];

    if response.reports.is_empty() {
        lines.push("No reconciliation reports found".to_string());
        return lines.join("\n");
    }

    lines.push("ID | Generated | Period Start | Period End | DB | Chain | Discrepancies".to_string());
    lines.push("-- | --------- | ------------ | ---------- | -- | ----- | -------------".to_string());

    for report in &response.reports {
        lines.push(format!(
            "{} | {} | {} | {} | {} | {} | {}",
            report.id,
            report.generated_at,
            report.period_start,
            report.period_end,
            report.total_db_transactions,
            report.total_chain_payments,
            yes_no(report.has_discrepancies)
        ));
    }

    lines.join("\n")
}

fn format_report_table(report: &ReportDetailResponse) -> String {
    let mut lines = vec![
        format!("Report ID: {}", report.id),
        format!("Generated: {}", report.generated_at),
        format!("Period: {} to {}", report.period_start, report.period_end),
        String::new(),
        "Summary:".to_string(),
        format!("  Database transactions: {}", report.summary.total_db_transactions),
        format!("  Chain payments: {}", report.summary.total_chain_payments),
        format!("  Missing on chain: {}", report.summary.missing_on_chain_count),
        format!("  Orphaned payments: {}", report.summary.orphaned_payments_count),
        format!("  Amount mismatches: {}", report.summary.amount_mismatches_count),
        format!("  Has discrepancies: {}", yes_no(report.summary.has_discrepancies)),
    ];

    if report.missing_on_chain.is_empty()
        && report.orphaned_payments.is_empty()
        && report.amount_mismatches.is_empty()
    {
        lines.push(String::new());
        lines.push("No discrepancies found".to_string());
    }

    lines.join("\n")
}

fn format_run_table(response: &RunResponse) -> String {
    let report = &response.report;
    [
        response.message.clone(),
        String::new(),
        format!("Report ID: {}", report.id),
        format!("Generated: {}", report.generated_at),
        format!("Period: {} to {}", report.period_start, report.period_end),
        String::new(),
        "Summary:".to_string(),
        format!("  Database transactions: {}", report.total_db_transactions),
        format!("  Chain payments: {}", report.total_chain_payments),
        format!("  Missing on chain: {}", report.missing_on_chain_count),
        format!("  Orphaned payments: {}", report.orphaned_payments_count),
        format!("  Amount mismatches: {}", report.amount_mismatches_count),
        format!("  Has discrepancies: {}", yes_no(report.has_discrepancies)),
    ]
    .join("\n")
}

// ── Settlement administration ─────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum AdminSettlementCommands {
    /// Update a settlement's status.
    #[command(
        name = "update-status",
        about = "Update a settlement's status",
        long_about = "Update a settlement's status through the admin API and print the updated settlement.\n\nRequired arguments:\n  <SETTLEMENT_ID>        UUID of the settlement to update.\nRequired flags:\n  --status <STATUS>      New status to apply (pending, completed, pending_review, disputed, adjusted, or voided).\nOptional flags:\n  --reason <REASON>      Human-readable reason for the change.\n  --new-total <TOTAL>    Replacement total amount; only meaningful when setting status to adjusted.\n  --actor <ACTOR>        Actor recorded in the audit log (default: admin).\n  --json                 Print the raw API response as JSON."
    )]
    UpdateStatus {
        /// UUID of the settlement to update.
        #[arg(value_name = "SETTLEMENT_ID")]
        settlement_id: Uuid,

        /// New status to apply.
        #[arg(long, value_name = "STATUS")]
        status: String,

        /// Human-readable reason for the change.
        #[arg(long, value_name = "REASON")]
        reason: Option<String>,

        /// Replacement total amount; only meaningful when setting status to adjusted.
        #[arg(long = "new-total", value_name = "TOTAL")]
        new_total: Option<String>,

        /// Actor recorded in the audit log.
        #[arg(long, value_name = "ACTOR", default_value = "admin")]
        actor: String,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Serialize)]
struct UpdateSettlementStatusRequest<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_total: Option<&'a str>,
    actor: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
struct SettlementResponse {
    id: Uuid,
    asset_code: String,
    total_amount: String,
    tx_count: i32,
    period_start: String,
    period_end: String,
    status: String,
    created_at: String,
    updated_at: String,
    dispute_reason: Option<String>,
    original_total_amount: Option<String>,
    reviewed_by: Option<String>,
    reviewed_at: Option<String>,
}

fn format_settlement_table(settlement: &SettlementResponse) -> String {
    [
        "Settlement updated successfully".to_string(),
        String::new(),
        format!("Settlement ID: {}", settlement.id),
        format!("Asset code: {}", settlement.asset_code),
        format!("Status: {}", settlement.status),
        format!("Total amount: {}", settlement.total_amount),
        format!("Tx count: {}", settlement.tx_count),
        format!("Period: {} to {}", settlement.period_start, settlement.period_end),
        format!(
            "Dispute reason: {}",
            settlement.dispute_reason.as_deref().unwrap_or("not provided")
        ),
        format!(
            "Original total amount: {}",
            settlement
                .original_total_amount
                .as_deref()
                .unwrap_or("not provided")
        ),
        format!(
            "Reviewed by: {}",
            settlement.reviewed_by.as_deref().unwrap_or("not provided")
        ),
        format!(
            "Reviewed at: {}",
            settlement.reviewed_at.as_deref().unwrap_or("not provided")
        ),
        format!("Created at: {}", settlement.created_at),
        format!("Updated at: {}", settlement.updated_at),
    ]
    .join("\n")
}

async fn handle_admin_settlements(base_url: &str, api_key: &str, command: AdminSettlementCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        AdminSettlementCommands::UpdateStatus {
            settlement_id,
            status,
            reason,
            new_total,
            actor,
            json,
        } => {
            let response: SettlementResponse = client
                .patch_json(
                    &format!("/admin/settlements/{settlement_id}/status"),
                    &UpdateSettlementStatusRequest {
                        status: &status,
                        reason: reason.as_deref(),
                        new_total: new_total.as_deref(),
                        actor: &actor,
                    },
                )
                .await?;
            println!("{}", output::render(&response, json, format_settlement_table)?);
        }
    }

    Ok(())
}

// ── Distributed locks ────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum LockCommands {
    #[command(
        about = "List active distributed locks",
        long_about = "List active distributed locks currently held by this Synapse instance.\n\nRequired flags: none.\nOptional flags:\n  --json            Print the raw API response as pretty JSON instead of the default table.\n\nOutput fields:\n  resource          Protected resource name for the lock.\n  token             Lock owner token.\n  acquired_at       Unix timestamp, in seconds, when the lock was acquired.\n  ttl_secs          Lock TTL in seconds.\n  expected_duration_secs  Expected lock hold duration in seconds.\n  overdue           Whether the lock has exceeded twice its expected duration."
    )]
    List {
        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct ListLocksResponse {
    active_locks: Vec<ActiveLockView>,
    total: usize,
    overdue: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ActiveLockView {
    resource: String,
    token: String,
    acquired_at: u64,
    ttl_secs: u64,
    expected_duration_secs: u64,
    overdue: bool,
}

async fn handle_locks(base_url: &str, api_key: &str, command: LockCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        LockCommands::List { json } => {
            let response: ListLocksResponse = client.get("/admin/locks").await?;
            println!("{}", output::render(&response, json, format_locks_table)?);
        }
    }

    Ok(())
}

fn format_locks_table(response: &ListLocksResponse) -> String {
    let mut lines = vec![format!(
        "Active locks: {} total ({} overdue)",
        response.total, response.overdue
    )];

    if response.active_locks.is_empty() {
        lines.push("No active locks found".to_string());
        return lines.join("\n");
    }

    lines.push("Resource | Token | Acquired At | TTL | Expected Duration | Overdue".to_string());
    lines.push("-------- | ----- | ----------- | --- | ----------------- | -------".to_string());

    for lock in &response.active_locks {
        lines.push(format!(
            "{} | {} | {} | {} | {} | {}",
            lock.resource,
            lock.token,
            lock.acquired_at,
            lock.ttl_secs,
            lock.expected_duration_secs,
            yes_no(lock.overdue)
        ));
    }

    lines.join("\n")
}

// ── Tenant quotas ────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum QuotaCommands {
    /// List quota usage for all active tenants.
    List {
        #[arg(long)]
        json: bool,
    },

    /// Get quota usage for one tenant.
    Get {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },

    /// Set a tenant quota override.
    Set {
        tenant_id: Uuid,

        /// Positive quota limit.
        #[arg(value_name = "LIMIT", conflicts_with = "limit_flag")]
        limit: Option<u32>,

        /// Positive quota limit.
        #[arg(long = "limit", value_name = "LIMIT")]
        limit_flag: Option<u32>,

        #[arg(long)]
        json: bool,
    },

    /// Reset the current usage counter for one tenant.
    Reset {
        tenant_id: Uuid,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct TenantQuotaView {
    tenant_id: Uuid,
    name: String,
    rate_limit_per_minute: i32,
    quota_status: Option<serde_json::Value>,
}

async fn handle_quotas(base_url: &str, api_key: &str, command: QuotaCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        QuotaCommands::List { json } => {
            let response: Vec<TenantQuotaView> = client.get("/admin/quotas").await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
        QuotaCommands::Get { tenant_id, json } => {
            let response: TenantQuotaView = client.get(&format!("/admin/quotas/{tenant_id}")).await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
        QuotaCommands::Set {
            tenant_id,
            limit,
            limit_flag,
            json: as_json,
        } => {
            let Some(limit) = limit.or(limit_flag) else {
                bail!("quota limit is required");
            };

            if limit == 0 {
                bail!("quota limit must be positive");
            }

            let response: serde_json::Value = client
                .put_json(
                    &format!("/admin/quotas/{tenant_id}"),
                    serde_json::json!({ "custom_limit": limit }),
                )
                .await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(as_json))?
            );
        }
        QuotaCommands::Reset { tenant_id, json } => {
            let response: serde_json::Value = client.delete(&format!("/admin/quotas/{tenant_id}/reset")).await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
    }

    Ok(())
}

// ── Bulk transaction administration ──────────────────────────────────────────

#[derive(Subcommand)]
pub enum TransactionAdminCommands {
    #[command(
        name = "bulk-status",
        about = "Bulk-update transaction statuses",
        long_about = "Bulk-update the status of multiple transactions.\n\nRequired flags:\n  --ids <IDS>       Comma-separated transaction IDs to update.\n  --status <STATUS> New status to apply to every listed transaction.\n\nOptional flags:\n  --format <FORMAT> Output format: table (default) or json."
    )]
    BulkStatus {
        #[arg(long, value_name = "IDS")]
        ids: String,

        #[arg(long, value_name = "STATUS")]
        status: String,

        #[arg(long, value_name = "FORMAT", default_value = "table")]
        format: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct BulkStatusResponse {
    updated: usize,
    failed: usize,
    errors: Vec<BulkStatusError>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BulkStatusError {
    transaction_id: String,
    error: String,
}

async fn handle_transactions(base_url: &str, api_key: &str, command: TransactionAdminCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        TransactionAdminCommands::BulkStatus { ids, status, format } => {
            let ids: Vec<String> = ids
                .split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect();

            let body = serde_json::json!({ "transaction_ids": ids, "status": status });
            let response: BulkStatusResponse =
                client.post_json("/admin/transactions/bulk-status", &body).await?;

            if format.eq_ignore_ascii_case("json") {
                println!("{}", serde_json::to_string_pretty(&response)?);
            } else {
                println!("updated: {}\nfailed: {}", response.updated, response.failed);
            }
        }
    }

    Ok(())
}

// ── Events (admin) ────────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum AdminEventsCommands {
    /// Fetch recent transaction status-change events (`GET /events`).
    ///
    /// Exit codes:
    ///   0 - Success (including empty list)
    ///   1 - Server error or network failure
    Watch {
        #[arg(long)]
        json: bool,
    },

    /// Attempt to reconnect a WebSocket session (`POST /reconnect`).
    Reconnect {
        #[arg(long, value_name = "CURSOR")]
        cursor: String,

        #[arg(long)]
        json: bool,
    },

    /// Check reconnection status without committing an attempt (`GET /reconnect/status`).
    #[command(name = "reconnect-status")]
    ReconnectStatus {
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,

        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct Event {
    transaction_id: Uuid,
    status: String,
    timestamp: String,
    message: Option<String>,
}

/// Top-level response for `POST /reconnect` and `GET /reconnect/status`.
#[derive(Debug, Deserialize, Serialize)]
struct ReconnectResponse {
    #[serde(rename = "type")]
    kind: String,
    status: Option<ReconnectStatusPayload>,
    backoff_seconds: Option<u64>,
    requires_resync: Option<bool>,
    message: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ReconnectStatusPayload {
    Ready { session_id: String },
    RetryAfter { wait_seconds: u64 },
    SessionExpired,
    InvalidToken,
}

impl ReconnectStatusPayload {
    fn label(&self) -> &str {
        match self {
            ReconnectStatusPayload::Ready { .. } => "ready",
            ReconnectStatusPayload::RetryAfter { .. } => "retry_after",
            ReconnectStatusPayload::SessionExpired => "session_expired",
            ReconnectStatusPayload::InvalidToken => "invalid_token",
        }
    }

    fn session_id(&self) -> Option<&str> {
        match self {
            ReconnectStatusPayload::Ready { session_id } => Some(session_id.as_str()),
            _ => None,
        }
    }
}

async fn handle_events(base_url: &str, api_key: &str, command: AdminEventsCommands) -> Result<()> {
    let client = AdminClient::new(base_url, api_key);

    match command {
        AdminEventsCommands::Watch { json } => {
            let events: Vec<Event> = client.get("/events").await?;
            println!("{}", output::render(&events, json, format_events_table)?);
        }

        AdminEventsCommands::Reconnect { cursor, json } => {
            let body = serde_json::json!({ "session_id": cursor });
            let response: ReconnectResponse = client.post_json("/reconnect", &body).await?;
            println!("{}", output::render(&response, json, format_reconnect_table)?);
        }

        AdminEventsCommands::ReconnectStatus { cursor, json } => {
            let path = "/reconnect/status";
            let response: ReconnectResponse = match cursor {
                Some(token) => {
                    let url = format!("{}?token={}", path, urlencode(&token));
                    client.get(&url).await?
                }
                None => client.get(path).await?,
            };
            println!("{}", output::render(&response, json, format_reconnect_table)?);
        }
    }

    Ok(())
}

/// Minimal percent-encoding for a single query value (avoids pulling in an
/// extra crate dependency just for this one call site).
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn format_events_table(events: &Vec<Event>) -> String {
    if events.is_empty() {
        return "No events".to_string();
    }

    let mut lines = vec![
        "TRANSACTION ID | STATUS | TIMESTAMP | MESSAGE".to_string(),
        "-------------- | ------ | --------- | -------".to_string(),
    ];

    for ev in events {
        lines.push(format!(
            "{} | {} | {} | {}",
            ev.transaction_id,
            ev.status,
            ev.timestamp,
            ev.message.as_deref().unwrap_or("-"),
        ));
    }

    lines.join("\n")
}

fn format_reconnect_table(response: &ReconnectResponse) -> String {
    let status_label = response.status.as_ref().map(|s| s.label()).unwrap_or("-");
    let session_id = response
        .status
        .as_ref()
        .and_then(|s| s.session_id())
        .unwrap_or("-");
    let backoff = response
        .backoff_seconds
        .map(|b| b.to_string())
        .unwrap_or_else(|| "-".to_string());
    let resync = response
        .requires_resync
        .map(|r| r.to_string())
        .unwrap_or_else(|| "-".to_string());
    let msg = response.message.as_deref().unwrap_or("-");

    [
        format!("TYPE             {}", response.kind),
        format!("STATUS           {status_label}"),
        format!("SESSION ID       {session_id}"),
        format!("BACKOFF (s)      {backoff}"),
        format!("REQUIRES RESYNC  {resync}"),
        format!("MESSAGE          {msg}"),
    ]
    .join("\n")
}
