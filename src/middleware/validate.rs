use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use jsonschema::JSONSchema;
use serde_json::{json, Value};

/// Validation error response
#[derive(Debug, serde::Serialize)]
struct ValidationErrorResponse {
    error: String,
    details: Vec<ValidationDetail>,
}

#[derive(Debug, serde::Serialize)]
struct ValidationDetail {
    field: String,
    message: String,
}

/// Validate request body against JSON schema
pub async fn validate_with_schema(
    schema: &'static JSONSchema,
    request: Request<Body>,
    next: Next<Body>,
) -> Response {
    // Extract body
    let (parts, body) = request.into_parts();

    let bytes = match hyper::body::to_bytes(body).await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "Failed to read request body",
                    "details": [{"field": "body", "message": e.to_string()}]
                })),
            )
                .into_response();
        }
    };

    // Parse JSON
    let payload: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "Invalid JSON",
                    "details": [{"field": "body", "message": e.to_string()}]
                })),
            )
                .into_response();
        }
    };

    // Validate against schema
    if let Err(errors) = schema.validate(&payload) {
        let details: Vec<ValidationDetail> = errors
            .map(|e| ValidationDetail {
                field: e.instance_path.to_string(),
                message: e.to_string(),
            })
            .collect();

        return (
            StatusCode::BAD_REQUEST,
            Json(ValidationErrorResponse {
                error: "Payload validation failed".to_string(),
                details,
            }),
        )
            .into_response();
    }

    // Reconstruct request with original body (convert Bytes to Vec<u8>)
    let request = Request::from_parts(parts, Body::from(bytes.to_vec()));
    next.run(request).await
}

/// Middleware factory for callback endpoint validation
pub async fn validate_callback(request: Request<Body>, next: Next<Body>) -> Response {
    validate_with_schema(
        &crate::validation::schemas::SCHEMAS.callback_v1,
        request,
        next,
    )
    .await
}

/// Middleware factory for webhook endpoint validation
pub async fn validate_webhook(request: Request<Body>, next: Next<Body>) -> Response {
    validate_with_schema(
        &crate::validation::schemas::SCHEMAS.webhook_v1,
        request,
        next,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
        Router,
    };
    use tower::ServiceExt;

    async fn test_handler(Json(payload): Json<Value>) -> impl IntoResponse {
        (StatusCode::OK, Json(payload))
    }

    #[tokio::test]
    async fn test_validate_callback_valid_payload() {
        let app = Router::new()
            .route("/callback", post(test_handler))
            .layer(axum::middleware::from_fn(validate_callback));

        let payload = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_validate_callback_missing_required_field() {
        let app = Router::new()
            .route("/callback", post(test_handler))
            .layer(axum::middleware::from_fn(validate_callback));

        let payload = json!({
            "amount": "100.50",
            "asset_code": "USD"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_validate_callback_invalid_stellar_account() {
        let app = Router::new()
            .route("/callback", post(test_handler))
            .layer(axum::middleware::from_fn(validate_callback));

        let payload = json!({
            "stellar_account": "INVALID",
            "amount": "100.50",
            "asset_code": "USD"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_validate_callback_additional_properties() {
        let app = Router::new()
            .route("/callback", post(test_handler))
            .layer(axum::middleware::from_fn(validate_callback));

        let payload = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD",
            "unknown_field": "value"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_validate_callback_invalid_json() {
        let app = Router::new()
            .route("/callback", post(test_handler))
            .layer(axum::middleware::from_fn(validate_callback));

        let request = Request::builder()
            .method("POST")
            .uri("/callback")
            .header("content-type", "application/json")
            .body(Body::from("invalid json"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_validate_webhook_valid_payload() {
        let app = Router::new()
            .route("/webhook", post(test_handler))
            .layer(axum::middleware::from_fn(validate_webhook));

        let payload = json!({
            "id": "webhook-123"
        });

        let request = Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
