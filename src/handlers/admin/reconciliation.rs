use crate::handlers::pagination::{validate_limit, validate_offset, DEFAULT_LIMIT, MAX_LIMIT};
use crate::services::reconciliation::{ReconciliationReport, ReconciliationService};
use crate::stellar::HorizonClient;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ListReportsQuery {
    #[serde(default = "default_limit")]
    limit: Option<i32>,
    #[serde(default)]
    offset: Option<i32>,
}

fn default_limit() -> Option<i32> {
    Some(DEFAULT_LIMIT)
}

#[derive(Debug, Serialize)]
pub struct ReconciliationReportSummary {
    pub id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_db_transactions: i32,
    pub total_chain_payments: i32,
    pub missing_on_chain_count: i32,
    pub orphaned_payments_count: i32,
    pub amount_mismatches_count: i32,
    pub has_discrepancies: bool,
}

impl
    From<(
        Uuid,
        DateTime<Utc>,
        DateTime<Utc>,
        DateTime<Utc>,
        i32,
        i32,
        i32,
        i32,
        i32,
        bool,
    )> for ReconciliationReportSummary
{
    fn from(
        fields: (
            Uuid,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
            i32,
            i32,
            i32,
            i32,
            i32,
            bool,
        ),
    ) -> Self {
        Self {
            id: fields.0,
            generated_at: fields.1,
            period_start: fields.2,
            period_end: fields.3,
            total_db_transactions: fields.4,
            total_chain_payments: fields.5,
            missing_on_chain_count: fields.6,
            orphaned_payments_count: fields.7,
            amount_mismatches_count: fields.8,
            has_discrepancies: fields.9,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListReportsResponse {
    pub reports: Vec<ReconciliationReportSummary>,
    pub total: i64,
    pub limit: i32,
    pub offset: i32,
}

#[derive(Debug, Deserialize)]
pub struct RunReconciliationRequest {
    pub account: String,
    #[serde(default)]
    period_hours: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct RunReconciliationResponse {
    pub message: String,
    pub report: ReconciliationReportSummary,
}

pub fn reconciliation_routes() -> Router<ApiState> {
    Router::new()
        .route("/reports", get(list_reconciliation_reports))
        .route("/reports/:id", get(get_reconciliation_report))
        .route("/run", post(run_reconciliation))
}

pub async fn list_reconciliation_reports(
    State(state): State<ApiState>,
    Query(query): Query<ListReportsQuery>,
) -> impl IntoResponse {
    let limit = match validate_limit(query.limit) {
        Ok(limit) => limit,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": message,
                    "max_limit": MAX_LIMIT
                })),
            )
                .into_response()
        }
    };

    let offset = match validate_offset(query.offset) {
        Ok(offset) => offset,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": message })),
            )
                .into_response()
        }
    };

    let pool = &state.app_state.db;

    let reports = sqlx::query_as::<
        _,
        (
            Uuid,
            DateTime<Utc>,
            DateTime<Utc>,
            DateTime<Utc>,
            i32,
            i32,
            i32,
            i32,
            i32,
            bool,
        ),
    >(
        r#"
        SELECT id, generated_at, period_start, period_end,
               total_db_transactions, total_chain_payments,
               missing_on_chain_count, orphaned_payments_count,
               amount_mismatches_count, has_discrepancies
        FROM reconciliation_reports
        ORDER BY generated_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await;

    match reports {
        Ok(rows) => {
            let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_reports")
                .fetch_one(pool)
                .await
                .unwrap_or(0);

            let summaries: Vec<ReconciliationReportSummary> = rows
                .into_iter()
                .map(ReconciliationReportSummary::from)
                .collect();

            (
                StatusCode::OK,
                Json(ListReportsResponse {
                    reports: summaries,
                    total,
                    limit,
                    offset,
                }),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Failed to list reconciliation reports: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve reconciliation reports"
                })),
            )
                .into_response()
        }
    }
}

pub async fn get_reconciliation_report(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    let pool = &state.app_state.db;

    let result = sqlx::query(
        r#"
        SELECT id, generated_at, period_start, period_end,
               total_db_transactions, total_chain_payments,
               missing_on_chain_count, orphaned_payments_count,
               amount_mismatches_count, has_discrepancies, report_json
        FROM reconciliation_reports
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await;

    match result {
        Ok(Some(row)) => {
            let report_json: serde_json::Value = row.try_get("report_json").unwrap_or_default();
            let full_report: ReconciliationReport = match serde_json::from_value(report_json) {
                Ok(r) => r,
                Err(_) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": "Failed to parse reconciliation report"
                        })),
                    )
                        .into_response()
                }
            };

            #[derive(Serialize)]
            struct ReportDetail {
                id: Uuid,
                generated_at: DateTime<Utc>,
                period_start: DateTime<Utc>,
                period_end: DateTime<Utc>,
                summary: ReportSummary,
                missing_on_chain: Vec<MissingTransactionOutput>,
                orphaned_payments: Vec<OrphanedPaymentOutput>,
                amount_mismatches: Vec<AmountMismatchOutput>,
            }

            #[derive(Serialize)]
            struct ReportSummary {
                total_db_transactions: usize,
                total_chain_payments: usize,
                missing_on_chain_count: i32,
                orphaned_payments_count: i32,
                amount_mismatches_count: i32,
                has_discrepancies: bool,
            }

            #[derive(Serialize)]
            struct MissingTransactionOutput {
                id: Uuid,
                stellar_account: String,
                amount: String,
                asset_code: String,
                memo: Option<String>,
                created_at: DateTime<Utc>,
            }

            #[derive(Serialize)]
            struct OrphanedPaymentOutput {
                payment_id: String,
                from: String,
                to: String,
                amount: String,
                asset_code: String,
                memo: Option<String>,
            }

            #[derive(Serialize)]
            struct AmountMismatchOutput {
                transaction_id: Uuid,
                payment_id: String,
                db_amount: String,
                chain_amount: String,
                memo: Option<String>,
            }

            let missing: Vec<MissingTransactionOutput> = full_report
                .missing_on_chain
                .iter()
                .map(|m| MissingTransactionOutput {
                    id: m.id,
                    stellar_account: m.stellar_account.clone(),
                    amount: m.amount.clone(),
                    asset_code: m.asset_code.clone(),
