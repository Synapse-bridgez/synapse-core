use crate::error::ApiError;
use crate::services::compliance::{ComplianceService, ComplianceStatus};
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
    pub status: Option<String>,
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

    let service = ComplianceService::new(state.app_state.db);
    match service.generate_report(&params.period).await {
        Ok(report) => (StatusCode::CREATED, Json(serde_json::json!(report))).into_response(),
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

    let status = match params.status.as_deref() {
        Some(raw) => match ComplianceStatus::parse(raw) {
            Some(status) => Some(status),
            None => {
                let allowed = ComplianceStatus::allowed_values().join(", ");
                let err = ApiError::BadRequest(format!(
                    "invalid status '{raw}'; allowed values: {allowed}"
                ));
                return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": err.to_string() })))
                    .into_response();
            }
        },
        None => None,
    };

    let service = ComplianceService::new(state.app_state.db);
    match service
        .list_reports(params.period.as_deref(), status, params.limit, params.offset)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_status_is_rejected() {
        let raw = "bogus";
        assert!(ComplianceStatus::parse(raw).is_none());
        let allowed = ComplianceStatus::allowed_values().join(", ");
        let err = ApiError::BadRequest(format!(
            "invalid status '{raw}'; allowed values: {allowed}"
        ));
        assert!(err.to_string().contains("bogus"));
        assert!(err.to_string().contains("allowed values"));
    }
}
