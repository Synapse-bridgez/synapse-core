use crate::services::compliance::ComplianceService;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct GenerateQuery {
    pub period: String,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub period: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Deserialize)]
pub struct ApproveReportRequest {
    pub reviewer_id: Uuid,
    pub reviewer_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RejectReportRequest {
    pub reviewer_id: Uuid,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct ReportSignoffResponse {
    pub id: String,
    pub status: Option<String>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<String>,
    pub reviewer_notes: Option<String>,
}

fn default_limit() -> i64 {
    20
}

pub async fn generate_report(
    State(state): State<ApiState>,
    Query(params): Query<GenerateQuery>,
) -> impl IntoResponse {
    crate::metrics::admin_compliance_report_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "generate")]);

    let service = ComplianceService::new(state.app_state.db.clone());
    match service.generate_report(&params.period).await {
        Ok(report) => {
            if let Err(e) = crate::telemetry::data_export::record_compliance_export(
                &state.app_state.db,
                "compliance_report",
                report.id,
                "admin",
                serde_json::json!({ "period": params.period }),
            )
            .await
            {
                tracing::error!("Failed to record compliance export telemetry: {e}");
            }
            (StatusCode::CREATED, Json(serde_json::json!(report))).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn list_reports(
    State(state): State<ApiState>,
    Query(params): Query<ListQuery>,
) -> impl IntoResponse {
    crate::metrics::admin_compliance_report_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "list")]);

    let service = ComplianceService::new(state.app_state.db);
    match service
        .list_reports(params.period.as_deref(), params.limit, params.offset)
        .await
    {
        Ok(reports) => (StatusCode::OK, Json(serde_json::json!(reports))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn approve_report(
    State(state): State<ApiState>,
    Path(report_id): Path<Uuid>,
    Json(payload): Json<ApproveReportRequest>,
) -> impl IntoResponse {
    crate::metrics::admin_compliance_report_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "approve")]);

    let service = ComplianceService::new(state.app_state.db);
    match service
        .approve_report(report_id, payload.reviewer_id, payload.reviewer_notes)
        .await
    {
        Ok(report) => {
            let response = ReportSignoffResponse {
                id: report.id.to_string(),
                status: report.status,
                reviewed_by: report.reviewed_by.map(|id| id.to_string()),
                reviewed_at: report.reviewed_at.map(|dt| dt.to_rfc3339()),
                reviewer_notes: report.reviewer_notes,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn reject_report(
    State(state): State<ApiState>,
    Path(report_id): Path<Uuid>,
    Json(payload): Json<RejectReportRequest>,
) -> impl IntoResponse {
    crate::metrics::admin_compliance_report_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "reject")]);

    let service = ComplianceService::new(state.app_state.db);
    match service
        .reject_report(report_id, payload.reviewer_id, payload.reason)
        .await
    {
        Ok(report) => {
            let response = ReportSignoffResponse {
                id: report.id.to_string(),
                status: report.status,
                reviewed_by: report.reviewed_by.map(|id| id.to_string()),
                reviewed_at: report.reviewed_at.map(|dt| dt.to_rfc3339()),
                reviewer_notes: report.reviewer_notes,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}
