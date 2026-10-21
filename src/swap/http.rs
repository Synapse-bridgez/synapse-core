//! HTTP surface of the Swap Engine.
//!
//! | Method | Path | Body | Response |
//! |--------|------|------|----------|
//! | POST | `/swap/quotes` | [`QuoteRequest`] | [`Quote`] (includes the route) |
//! | GET  | `/swap/quotes/:id` | | [`Quote`] |
//! | POST | `/swap/executions` | [`ExecuteRequest`] | [`SwapRecord`] |
//! | GET  | `/swap/executions/:id` | | [`SwapRecord`] |
//! | GET  | `/swap/executions/:id/events` | | `[SwapEvent]` |
//!
//! Errors are `{"error": {"code": "...", "message": "..."}}` with the status
//! from [`EngineError::http_status`]. Amounts in request bodies must be JSON
//! strings (see [`crate::swap::decimal`]).
//!
//! The router is self-contained (`Router<()>`); mount it with
//! `app.merge(swap::http::router(service))` behind whatever auth layer the
//! deployment uses.

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::de::DeserializeOwned;
use std::sync::Arc;

use crate::swap::engine::{EngineError, ExecuteRequest, QuoteRequest, SwapService};

pub fn router(service: Arc<SwapService>) -> Router {
    Router::new()
        .route("/swap/quotes", post(create_quote))
        .route("/swap/quotes/:id", get(get_quote))
        .route("/swap/executions", post(execute))
        .route("/swap/executions/:id", get(get_swap))
        .route("/swap/executions/:id/events", get(get_events))
        .with_state(service)
}

struct ApiError(EngineError);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::json!({
            "error": { "code": self.0.code(), "message": self.0.to_string() }
        });
        (status, Json(body)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        ApiError(e)
    }
}

fn parse<T: DeserializeOwned>(body: &Bytes) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError(EngineError::InvalidRequest(e.to_string())))
}

async fn create_quote(
    State(svc): State<Arc<SwapService>>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let req: QuoteRequest = parse(&body)?;
    Ok(Json(svc.quote(req).await?))
}

async fn get_quote(
    State(svc): State<Arc<SwapService>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(svc.get_quote(&id).await?))
}

async fn execute(
    State(svc): State<Arc<SwapService>>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let req: ExecuteRequest = parse(&body)?;
    Ok(Json(svc.execute(req).await?))
}

async fn get_swap(
    State(svc): State<Arc<SwapService>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(svc.get_swap(&id).await?))
}

async fn get_events(
    State(svc): State<Arc<SwapService>>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(svc.events(&id).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::asset::{Asset, AssetRegistry};
    use crate::swap::fees::FeeSchedule;
    use crate::swap::liquidity::StaticRateProvider;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> Router {
        let svc = SwapService::builder(AssetRegistry::new(["USDC", "XLM"].map(Asset::stellar)))
            .provider(Arc::new(
                StaticRateProvider::new("lp", FeeSchedule::bps(0)).with_rate("USDC", "XLM", "10"),
            ))
            .build();
        router(Arc::new(svc))
    }

    async fn post(app: Router, path: &str, body: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = hyper::body::to_bytes(resp.into_body()).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn quote_response_includes_route_and_string_amounts() {
        let (status, body) = post(
            app(),
            "/swap/quotes",
            r#"{"source_asset":"USDC","destination_asset":"XLM","amount_in":"2.5"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["amount_in"], "2.5000000");
        assert_eq!(body["expected_out"], "25.0000000");
        assert_eq!(body["route"]["path"], serde_json::json!(["USDC", "XLM"]));
        assert_eq!(body["route"]["hops"][0]["provider"], "lp");
    }

    #[tokio::test]
    async fn numeric_amount_is_a_400() {
        let (status, body) = post(
            app(),
            "/swap/quotes",
            r#"{"source_asset":"USDC","destination_asset":"XLM","amount_in":2.5}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn unattainable_min_received_is_a_422() {
        let (status, body) = post(
            app(),
            "/swap/quotes",
            r#"{"source_asset":"USDC","destination_asset":"XLM","amount_in":"1",
                "protection":{"min_received":"10.0000001"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "min_received_unattainable");
    }

    #[tokio::test]
    async fn unroutable_pair_is_a_422() {
        let (status, body) = post(
            app(),
            "/swap/quotes",
            r#"{"source_asset":"XLM","destination_asset":"USDC","amount_in":"1"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "no_route");
    }
}
