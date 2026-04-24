use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use crate::error::RequestId;

/// Middleware that enriches error responses with request_id from extensions.
///
/// This middleware intercepts responses and if they are error responses (4xx/5xx),
/// it injects the request_id into the JSON body.
pub async fn error_enrichment_middleware(
    req: Request<Body>,
    next: Next<Body>,
) -> Result<Response, StatusCode> {
    // Extract request_id from extensions
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|rid| rid.0.clone())
        .unwrap_or_else(|| "unknown".to_string());

    // Process the request
    let response = next.run(req).await;

    // Check if response is an error (4xx or 5xx)
    let status = response.status();
    if status.is_client_error() || status.is_server_error() {
        // Try to extract and modify the JSON body
        let (parts, body) = response.into_parts();

        // Convert body to bytes
        let bytes = match axum::body::to_bytes(body, usize::MAX).await {
            Ok(b) => b,
            Err(_) => {
                // If we can't read the body, just return the original response
                return Ok(Response::from_parts(parts, Body::empty()));
            }
        };

        // Try to parse as JSON
        if let Ok(mut json_value) = serde_json::from_slice::<Value>(&bytes) {
            // Inject request_id if it's an object
            if let Some(obj) = json_value.as_object_mut() {
                obj.insert("request_id".to_string(), json!(request_id));
            }

            // Serialize back to JSON
            let new_body = serde_json::to_vec(&json_value).unwrap_or(bytes.to_vec());
            return Ok(Response::from_parts(parts, Body::from(new_body)));
        }

        // If not JSON, return original response
        Ok(Response::from_parts(parts, Body::from(bytes)))
    } else {
        // Not an error response, pass through
        Ok(response)
    }
}
