use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::queries;
use crate::ApiState;

#[derive(Debug, Deserialize)]
pub struct GraphqlRequest {
    pub query: String,
    pub variables: Option<Value>,
}

pub async fn graphql_handler(
    State(state): State<ApiState>,
    Json(payload): Json<GraphqlRequest>,
) -> impl IntoResponse {
    let query = payload.query.replace(char::is_whitespace, "");

    if query.contains("transactions{") {
        let status_filter = payload
            .variables
            .as_ref()
            .and_then(|v| v.get("filter"))
            .and_then(|f| f.get("status"))
            .and_then(|s| s.as_str())
            .map(ToOwned::to_owned);

        match queries::list_transactions(&state.app_state.db, 100, None, false).await {
            Ok(mut rows) => {
                if let Some(status) = status_filter {
                    rows.retain(|t| t.status == status);
                }
                let data: Vec<Value> = rows
                    .into_iter()
                    .map(|t| json!({ "id": t.id.to_string(), "status": t.status }))
                    .collect();
                return (
                    StatusCode::OK,
                    Json(json!({ "data": { "transactions": data } })),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "errors": [{ "message": e.to_string() }] })),
                )
                    .into_response()
            }
        }
    }

    if query.starts_with("{transaction(id:\"") || query.contains("transaction(id:\"") {
        let id = extract_id(&payload.query);
        if let Some(id) = id {
            match queries::get_transaction(&state.app_state.db, id).await {
                Ok(t) => {
                    return (
                        StatusCode::OK,
                        Json(json!({
                            "data": {
                                "transaction": {
                                    "id": t.id.to_string(),
                                    "status": t.status,
                                    "amount": t.amount.to_string(),
                                    "assetCode": t.asset_code
                                }
                            }
                        })),
                    )
                        .into_response()
                }
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "errors": [{ "message": e.to_string() }] })),
                    )
                        .into_response()
                }
            }
        }
    }

    if query.contains("mutation{forceCompleteTransaction(id:\"") {
        let id = extract_id(&payload.query);
        if let Some(id) = id {
            let updated = sqlx::query(
                "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
            )
            .bind(id)
            .execute(&state.app_state.db)
            .await;

            if let Err(e) = updated {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "errors": [{ "message": e.to_string() }] })),
                )
                    .into_response();
            }

            match queries::get_transaction(&state.app_state.db, id).await {
                Ok(t) => {
                    return (StatusCode::OK, Json(json!({
                        "data": { "forceCompleteTransaction": { "id": t.id.to_string(), "status": t.status } }
                    }))).into_response()
                }
                Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "errors": [{ "message": e.to_string() }] }))).into_response(),
            }
        }
    }

    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "errors": [{ "message": "Unsupported GraphQL query" }] })),
    )
        .into_response()
}

fn extract_id(query: &str) -> Option<Uuid> {
    let marker = if query.contains("id: \"") {
        "id: \""
    } else {
        "id:\""
    };
    let start = query.find(marker)? + marker.len();
    let remainder = &query[start..];
    let end = remainder.find('"')?;
    Uuid::parse_str(&remainder[..end]).ok()
}
