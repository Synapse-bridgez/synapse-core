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
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct ListReportsQuery {
    #[serde(default = "default_limit")]
    limit: Option<i32>,
    #[serde(default)]
    offset: Option<i32>,
}

fn default_limit() -> i32 {
    20
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
    let limit = query.limit.unwrap_or(20);
    let offset = query.offset.unwrap_or(0);

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
                    memo: m.memo.clone(),
                    created_at: m.created_at,
                })
                .collect();

            let orphaned: Vec<OrphanedPaymentOutput> = full_report
                .orphaned_payments
                .iter()
                .map(|o| OrphanedPaymentOutput {
                    payment_id: o.payment_id.clone(),
                    from: o.from.clone(),
                    to: o.to.clone(),
                    amount: o.amount.clone(),
                    asset_code: o.asset_code.clone(),
                    memo: o.memo.clone(),
                })
                .collect();

            let mismatches: Vec<AmountMismatchOutput> = full_report
                .amount_mismatches
                .iter()
                .map(|a| AmountMismatchOutput {
                    transaction_id: a.transaction_id,
                    payment_id: a.payment_id.clone(),
                    db_amount: a.db_amount.clone(),
                    chain_amount: a.chain_amount.clone(),
                    memo: a.memo.clone(),
                })
                .collect();

            let report_id: Uuid = row.try_get("id").unwrap_or_default();
            let generated_at: DateTime<Utc> = row.try_get("generated_at").unwrap_or_default();
            let period_start: DateTime<Utc> = row.try_get("period_start").unwrap_or_default();
            let period_end: DateTime<Utc> = row.try_get("period_end").unwrap_or_default();
            let total_db: i32 = row.try_get("total_db_transactions").unwrap_or(0);
            let total_chain: i32 = row.try_get("total_chain_payments").unwrap_or(0);
            let missing_count: i32 = row.try_get("missing_on_chain_count").unwrap_or(0);
            let orphaned_count: i32 = row.try_get("orphaned_payments_count").unwrap_or(0);
            let mismatches_count: i32 = row.try_get("amount_mismatches_count").unwrap_or(0);
            let has_discrepancies: bool = row.try_get("has_discrepancies").unwrap_or(false);

            (
                StatusCode::OK,
                Json(ReportDetail {
                    id: report_id,
                    generated_at,
                    period_start,
                    period_end,
                    summary: ReportSummary {
                        total_db_transactions: total_db as usize,
                        total_chain_payments: total_chain as usize,
                        missing_on_chain_count: missing_count,
                        orphaned_payments_count: orphaned_count,
                        amount_mismatches_count: mismatches_count,
                        has_discrepancies,
                    },
                    missing_on_chain: missing,
                    orphaned_payments: orphaned,
                    amount_mismatches: mismatches,
                }),
            )
                .into_response()
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Reconciliation report not found"
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to get reconciliation report {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve reconciliation report"
                })),
            )
                .into_response()
        }
    }
}

pub async fn run_reconciliation(
    State(state): State<ApiState>,
    Json(payload): Json<RunReconciliationRequest>,
) -> impl IntoResponse {
    let account = payload.account;
    let period_hours = payload.period_hours.unwrap_or(24);

    if account.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "account is required"
            })),
        )
            .into_response();
    }

    let horizon_client = HorizonClient::new(state.app_state.horizon_client.base_url.clone());
    let pool = state.app_state.db.clone();

    let svc = ReconciliationService::new(horizon_client.clone(), pool.clone());

    let end = Utc::now();
    let start = end - Duration::hours(period_hours as i64);

    let report = match svc.reconcile(&account, start, end).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Reconciliation failed: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Reconciliation failed: {}", e)
                })),
            )
                .into_response();
        }
    };

    if let Err(e) = ReconciliationService::store_report(&pool, &report).await {
        tracing::error!("Failed to store reconciliation report: {}", e);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "Failed to store reconciliation report"
            })),
        )
            .into_response();
    }

    let summary = ReconciliationReportSummary::from((
        Uuid::new_v4(),
        report.generated_at,
        report.period_start,
        report.period_end,
        report.total_db_transactions as i32,
        report.total_chain_payments as i32,
        report.missing_on_chain.len() as i32,
        report.orphaned_payments.len() as i32,
        report.amount_mismatches.len() as i32,
        !report.missing_on_chain.is_empty()
            || !report.orphaned_payments.is_empty()
            || !report.amount_mismatches.is_empty(),
    ));

    (
        StatusCode::OK,
        Json(RunReconciliationResponse {
            message: "Reconciliation completed successfully".to_string(),
            report: summary,
        }),
    )
        .into_response()
}
