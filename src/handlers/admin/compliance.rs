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
