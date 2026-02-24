use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::db::queries;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct GraphqlRequest {
    pub query: String,
    pub variables: Option<Value>,
}

pub async fn graphql_handler(
    State(state): State<AppState>,
    Json(payload): Json<GraphqlRequest>,
) -> Json<Value> {
    let query = payload.query.replace(char::is_whitespace, "");

    if query.contains("transactions{") {
        let status_filter = payload
            .variables
            .as_ref()
            .and_then(|v| v.get("filter"))
            .and_then(|f| f.get("status"))
            .and_then(|s| s.as_str())
            .map(ToOwned::to_owned);

        match queries::list_transactions(&state.db, 100, None, false).await {
            Ok(mut rows) => {
                if let Some(status) = status_filter {
                    rows.retain(|t| t.status == status);
                }
                let data: Vec<Value> = rows
                    .into_iter()
                    .map(|t| json!({ "id": t.id.to_string(), "status": t.status }))
                    .collect();
                return Json(json!({ "data": { "transactions": data } }));
            }
            Err(e) => return Json(json!({ "errors": [{ "message": e.to_string() }] })),
        }
    }

    if query.starts_with("{transaction(id:\"") || query.contains("transaction(id:\"") {
        let id = extract_id(&payload.query);
        if let Some(id) = id {
            match queries::get_transaction(&state.db, id).await {
                Ok(t) => {
                    return Json(json!({
                        "data": {
                            "transaction": {
                                "id": t.id.to_string(),
                                "status": t.status,
                                "amount": t.amount.to_string(),
                                "assetCode": t.asset_code
                            }
                        }
                    }))
                }
                Err(e) => return Json(json!({ "errors": [{ "message": e.to_string() }] })),
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
            .execute(&state.db)
            .await;

            if let Err(e) = updated {
                return Json(json!({ "errors": [{ "message": e.to_string() }] }));
            }

            match queries::get_transaction(&state.db, id).await {
                Ok(t) => {
                    return Json(json!({
                        "data": { "forceCompleteTransaction": { "id": t.id.to_string(), "status": t.status } }
                    }))
                }
                Err(e) => return Json(json!({ "errors": [{ "message": e.to_string() }] })),
            }
        }
    }

    Json(json!({ "errors": [{ "message": "Unsupported GraphQL query" }] }))
}

fn extract_id(query: &str) -> Option<Uuid> {
    let marker = if query.contains("id: \"") { "id: \"" } else { "id:\"" };
    let start = query.find(marker)? + marker.len();
    let remainder = &query[start..];
    let end = remainder.find('"')?;
    Uuid::parse_str(&remainder[..end]).ok()
}
