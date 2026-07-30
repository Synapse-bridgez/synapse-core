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
                return Ok(parts.status.into_response());
            }
        };

        if let Ok(mut json_value) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(obj) = json_value.as_object_mut() {
                obj.insert("request_id".to_string(), json!(request_id));
            }
            let new_body = serde_json::to_vec(&json_value).unwrap_or_else(|_| bytes.to_vec());
            let mut resp = match Response::builder()
                .status(parts.status)
                .body(axum::body::boxed(axum::body::Full::from(new_body)))
            {
                Ok(resp) => resp,
                Err(_) => return Ok(parts.status.into_response()),
            };
            // Copy original headers, but drop Content-Length: the body is now
            // longer (it gained a "request_id" field) so the original value
            // would be stale and cause clients/proxies to truncate the body.
            let mut headers = parts.headers;
            headers.remove(axum::http::header::CONTENT_LENGTH);
            *resp.headers_mut() = headers;
            return Ok(resp);
        }

        let mut resp = match Response::builder()
            .status(parts.status)
            .body(axum::body::boxed(axum::body::Full::from(bytes)))
        {
            Ok(resp) => resp,
            Err(_) => return Ok(parts.status.into_response()),
        };
        // Same treatment for the non-JSON fallback path: drop Content-Length
        // so the framing stays correct even if the body was re-buffered.
        let mut headers = parts.headers;
        headers.remove(axum::http::header::CONTENT_LENGTH);
        *resp.headers_mut() = headers;
        Ok(resp)
    } else {
        Ok(response)
    }
}
