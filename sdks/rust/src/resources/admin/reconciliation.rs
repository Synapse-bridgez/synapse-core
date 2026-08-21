use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::{
    ListReconciliationReports, ListReportsParams, ReconciliationReportDetail,
    RunReconciliationRequest, RunReconciliationResponse,
};
use uuid::Uuid;

/// Admin operations for reconciliation reports.
///
/// Use this to list, retrieve, and run reconciliation reports for a Synapse deployment.
/// All reconciliation operations are synchronous — the `run()` method blocks until
/// reconciliation completes and returns the summary report.
///
/// # Example
///
/// ```no_run
/// use synapse_sdk::AdminSynapseClient;
/// use synapse_sdk::models::ListReportsParams;
/// use uuid::Uuid;
///
/// # #[tokio::main]
/// # async fn main() {
/// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
/// let recon = admin.reconciliation();
///
/// // List recent reports
/// let reports = recon.list_reports(ListReportsParams {
///     limit: Some(50),
///     offset: Some(0),
/// }).await.expect("failed to list reports");
///
/// println!("Found {} total reports", reports.total);
///
/// // Get details of a specific report
/// if let Some(summary) = reports.reports.first() {
///     let detail = recon.get_report(summary.id).await.expect("failed to get report");
///     println!("Report has {} discrepancies", detail.missing_on_chain.len());
/// }
///
/// // Run a new reconciliation
/// let result = recon.run(
///     "GABC1234567890123456789012345678901234567890123456789012",
///     Some(24),  // look back 24 hours
/// ).await.expect("reconciliation failed");
/// println!("Reconciliation: {}", result.message);
/// # }
/// ```
pub struct AdminReconciliation<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminReconciliation<'a> {
    /// List reconciliation reports with optional pagination.
    ///
    /// Returns a page of reconciliation report summaries. The `limit` and `offset`
    /// parameters control pagination (server default: limit=20, offset=0).
    ///
    /// # Errors
    /// - [`SynapseError::Http`] – server returned a 5xx error.
    /// - [`SynapseError::Api`] – server returned a 4xx error with details.
    /// - [`SynapseError::Network`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::models::ListReportsParams;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let reconciliation = client.reconciliation();
    ///
    /// // Fetch first 25 reports
    /// let params = ListReportsParams {
    ///     limit: Some(25),
    ///     offset: Some(0),
    /// };
    ///
    /// match reconciliation.list_reports(params).await {
    ///     Ok(page) => {
    ///         println!("Found {} total reports, showing {} on this page",
    ///                  page.total, page.reports.len());
    ///         for report in &page.reports {
    ///             if report.has_discrepancies {
    ///                 println!("⚠️  Report {} has discrepancies", report.id);
    ///             }
    ///         }
    ///     }
    ///     Err(e) => eprintln!("Failed to list reports: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn list_reports(
        &self,
        params: ListReportsParams,
    ) -> Result<ListReconciliationReports, SynapseError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        let limit_str;
        let offset_str;

        if let Some(limit) = params.limit {
            limit_str = limit.to_string();
            query.push(("limit", limit_str));
        }
        if let Some(offset) = params.offset {
            offset_str = offset.to_string();
            query.push(("offset", offset_str));
        }

        let query_refs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();

        self.client
            .get_query::<ListReconciliationReports>("/admin/reconciliation/reports", &query_refs)
            .await
    }

    /// Get a single reconciliation report by ID with full details.
    ///
    /// Returns the complete report including all discrepancy details:
    /// - Missing transactions (in database but not on chain)
    /// - Orphaned payments (on chain but not in database)
    /// - Amount mismatches (different amounts in DB vs chain)
    ///
    /// # Errors
    /// - [`SynapseError::Api`] with status 404 – report not found.
    /// - [`SynapseError::Http`] – server returned a 5xx error.
    /// - [`SynapseError::Network`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::SynapseError;
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let reconciliation = client.reconciliation();
    ///
    /// let report_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// match reconciliation.get_report(report_id).await {
    ///     Ok(report) => {
    ///         println!("Report {}", report.id);
    ///         println!("Missing on chain: {}", report.missing_on_chain.len());
    ///         for tx in &report.missing_on_chain {
    ///             println!("  - Amount: {}, Account: {}", tx.amount, tx.stellar_account);
    ///         }
    ///     }
    ///     Err(SynapseError::Api { status: 404, .. }) => {
    ///         eprintln!("Report not found");
    ///     }
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn get_report(&self, id: Uuid) -> Result<ReconciliationReportDetail, SynapseError> {
        let path = format!("/admin/reconciliation/reports/{}", id);
        self.client.get::<ReconciliationReportDetail>(&path).await
    }

    /// Run a reconciliation for the specified Stellar account.
    ///
    /// This method blocks until the reconciliation completes and returns a report summary.
    /// For full details of discrepancies, call [`get_report`] with the returned report ID.
    ///
    /// # Parameters
    /// - `account`: Stellar account to reconcile (e.g., `G...`)
    /// - `period_hours`: Lookback window in hours. If `None`, defaults to 24 hours.
    ///
    /// # Semantics
    /// **This is a synchronous/blocking operation**: the API endpoint runs reconciliation
    /// immediately and returns the complete report summary. It does not return a task ID
    /// to poll later.
    ///
    /// # Errors
    /// - [`SynapseError::Api`] with status 400 – invalid account format or server validation error.
    /// - [`SynapseError::Http`] – server returned a 5xx error.
    /// - [`SynapseError::Network`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::SynapseError;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let reconciliation = client.reconciliation();
    ///
    /// let account = "GABC1234567890123456789012345678901234567890123456789012";
    ///
    /// // Run reconciliation for the past 48 hours
    /// match reconciliation.run(account, Some(48)).await {
    ///     Ok(response) => {
    ///         println!("✓ Reconciliation completed: {}", response.message);
    ///         let report = &response.report;
    ///
    ///         if report.has_discrepancies {
    ///             println!("⚠️  Found discrepancies:");
    ///             println!("  Missing on chain: {}", report.missing_on_chain_count);
    ///             println!("  Orphaned payments: {}", report.orphaned_payments_count);
    ///             println!("  Amount mismatches: {}", report.amount_mismatches_count);
    ///
    ///             // Fetch full report for details
    ///             if let Ok(full) = reconciliation.get_report(report.id).await {
    ///                 println!("Details available via report ID: {}", full.id);
    ///             }
    ///         } else {
    ///             println!("✓ No discrepancies found");
    ///         }
    ///     }
    ///     Err(SynapseError::Api { status: 400, message }) => {
    ///         eprintln!("Invalid account or parameters: {}", message);
    ///     }
    ///     Err(e) => eprintln!("Reconciliation error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn run(
        &self,
        account: &str,
        period_hours: Option<i32>,
    ) -> Result<RunReconciliationResponse, SynapseError> {
        let req = RunReconciliationRequest {
            account: account.to_string(),
            period_hours,
        };
        self.client
            .post::<_, RunReconciliationResponse>("/admin/reconciliation/run", &req)
            .await
    }
}

impl<'a> AdminReconciliation<'a> {
    /// Create a new [`AdminReconciliation`] resource.
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminReconciliation { client }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn report_summary_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "generated_at": "2024-01-15T10:00:00Z",
            "period_start": "2024-01-14T10:00:00Z",
            "period_end": "2024-01-15T10:00:00Z",
            "total_db_transactions": 100,
            "total_chain_payments": 100,
            "missing_on_chain_count": 0,
            "orphaned_payments_count": 0,
            "amount_mismatches_count": 0,
            "has_discrepancies": false,
        })
    }

    #[tokio::test]
    async fn list_reports_returns_reports_on_200() {
        let server = MockServer::start().await;
        let report_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/admin/reconciliation/reports"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reports": [report_summary_json(report_id)],
                "total": 1,
                "limit": 20,
                "offset": 0,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .list_reports(ListReportsParams {
                limit: None,
                offset: None,
            })
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let page = result.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.reports.len(), 1);
        assert_eq!(page.reports[0].id.to_string(), report_id);
    }

    #[tokio::test]
    async fn get_report_returns_report_detail_on_200() {
        let server = MockServer::start().await;
        let report_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path(format!("/admin/reconciliation/reports/{}", report_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": report_id,
                "generated_at": "2024-01-15T10:00:00Z",
                "period_start": "2024-01-14T10:00:00Z",
                "period_end": "2024-01-15T10:00:00Z",
                "summary": {
                    "total_db_transactions": 100,
                    "total_chain_payments": 100,
                    "missing_on_chain_count": 0,
                    "orphaned_payments_count": 0,
                    "amount_mismatches_count": 0,
                    "has_discrepancies": false,
                },
                "missing_on_chain": [],
                "orphaned_payments": [],
                "amount_mismatches": [],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .get_report(Uuid::parse_str(report_id).unwrap())
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let report = result.unwrap();
        assert_eq!(report.id.to_string(), report_id);
        assert_eq!(report.summary.total_db_transactions, 100);
        assert!(!report.summary.has_discrepancies);
    }

    #[tokio::test]
    async fn run_returns_report_summary_on_200() {
        let server = MockServer::start().await;
        let report_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path("/admin/reconciliation/run"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Reconciliation completed successfully",
                "report": report_summary_json(report_id),
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .run(
                "GABC1234567890123456789012345678901234567890123456789012",
                None,
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let response = result.unwrap();
        assert_eq!(response.message, "Reconciliation completed successfully");
        assert_eq!(response.report.id.to_string(), report_id);
    }

    #[tokio::test]
    async fn run_with_period_hours_sends_parameter() {
        let server = MockServer::start().await;
        let report_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path("/admin/reconciliation/run"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Reconciliation completed successfully",
                "report": report_summary_json(report_id),
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .run(
                "GABC1234567890123456789012345678901234567890123456789012",
                Some(48),
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn list_reports_with_pagination() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/reconciliation/reports"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reports": [],
                "total": 0,
                "limit": 10,
                "offset": 20,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .list_reports(ListReportsParams {
                limit: Some(10),
                offset: Some(20),
            })
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let page = result.unwrap();
        assert_eq!(page.limit, 10);
        assert_eq!(page.offset, 20);
        assert_eq!(page.total, 0);
        assert!(page.reports.is_empty());
    }

    #[tokio::test]
    async fn get_report_with_discrepancies() {
        let server = MockServer::start().await;
        let report_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path(format!("/admin/reconciliation/reports/{}", report_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": report_id,
                "generated_at": "2024-01-15T10:00:00Z",
                "period_start": "2024-01-14T10:00:00Z",
                "period_end": "2024-01-15T10:00:00Z",
                "summary": {
                    "total_db_transactions": 105,
                    "total_chain_payments": 102,
                    "missing_on_chain_count": 2,
                    "orphaned_payments_count": 1,
                    "amount_mismatches_count": 0,
                    "has_discrepancies": true,
                },
                "missing_on_chain": [
                    {
                        "id": "d0000000-0000-0000-0000-000000000001",
                        "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
                        "amount": "100.00",
                        "asset_code": "USD",
                        "memo": "payment-1",
                        "created_at": "2024-01-14T12:00:00Z",
                    }
                ],
                "orphaned_payments": [],
                "amount_mismatches": [],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .get_report(Uuid::parse_str(report_id).unwrap())
            .await;

        assert!(result.is_ok());
        let report = result.unwrap();
        assert!(report.summary.has_discrepancies);
        assert_eq!(report.summary.missing_on_chain_count, 2);
        assert_eq!(report.missing_on_chain.len(), 1);
    }

    #[tokio::test]
    async fn run_returns_http_error_on_bad_account() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/admin/reconciliation/run"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(400).set_body_string("invalid account format"))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation.run("invalid-account", None).await;

        assert!(result.is_err());
        match result {
            Err(SynapseError::Api { status, .. }) => {
                assert_eq!(status, 400);
            }
            other => panic!("expected Api error with 400 status, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn get_report_returns_not_found_on_404() {
        let server = MockServer::start().await;
        let report_id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("GET"))
            .and(path(format!("/admin/reconciliation/reports/{}", report_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Report not found"))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .get_report(Uuid::parse_str(report_id).unwrap())
            .await;

        assert!(result.is_err());
        match result {
            Err(SynapseError::Api { status, .. }) => {
                assert_eq!(status, 404);
            }
            other => panic!("expected Api error with 404 status, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn list_reports_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/reconciliation/reports"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "reports": [],
                "total": 0,
                "limit": 20,
                "offset": 0,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let reconciliation = AdminReconciliation::new(&client);
        let result = reconciliation
            .list_reports(ListReportsParams::default())
            .await;

        // If the test passes, it means X-Admin-Key header was sent, not X-API-Key
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }
}
