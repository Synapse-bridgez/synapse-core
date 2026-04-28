use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use crate::error::RequestId;

/// Middleware that enriches error responses with request_id from extensions.
pub async fn error_enrichment_middleware(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, StatusCode> {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|rid| rid.0.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let response = next.run(req).await;

    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        let (parts, body) = response.into_parts();

        let bytes = match hyper::body::to_bytes(body).await {
            Ok(b) => b,
            Err(_) => {
                return Ok((parts.status, "").into_response());
            }
        };

        if let Ok(mut json_value) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(obj) = json_value.as_object_mut() {
                obj.insert("request_id".to_string(), json!(request_id));
            }
            let new_body = serde_json::to_vec(&json_value).unwrap_or_else(|_| bytes.to_vec());
            let mut resp = (parts.status, axum::body::Full::from(new_body)).into_response();
            *resp.headers_mut() = parts.headers;
            return Ok(resp);
        }

        let mut resp = (parts.status, axum::body::Full::from(bytes)).into_response();
        *resp.headers_mut() = parts.headers;
        Ok(resp)
    } else {
        Ok(response)
    }
}
