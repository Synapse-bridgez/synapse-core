use crate::services::compliance::ComplianceService;
use crate::ApiState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

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
