use axum::{
    http::{HeaderValue, Response, header, HeaderName},
    middleware::Next,
    response::IntoResponse,
};
use std::str::FromStr;

pub async fn inject_deprecation_headers<B>(
    req: axum::http::Request<B>,
    next: Next<B>,
) -> Response {
    let mut response = next.run(req).await;
    
    // Set Deprecation header to true
    response.headers_mut().insert(
        HeaderName::from_str("Deprecation").unwrap(),
        HeaderValue::from_static("true"),
    );
    
    // Set Sunset header (example date)
    response.headers_mut().insert(
        HeaderName::from_str("Sunset").unwrap(),
        HeaderValue::from_static("Fri, 31 Dec 2026 23:59:59 GMT"),
    );
    
    response
}
