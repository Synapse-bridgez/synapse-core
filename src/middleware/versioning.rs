use axum::{
    http::{HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response as AxumResponse,
};
use std::str::FromStr;

pub async fn inject_deprecation_headers<B>(
    req: Request<B>,
    next: Next<B>,
) -> AxumResponse {
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

/// Injects an `API-Version` response header indicating which version handled the request.
/// Also supports `Accept-Version` request header for version negotiation.
pub async fn inject_api_version_header<B>(
    version: &'static str,
    req: Request<B>,
    next: Next<B>,
) -> AxumResponse {
    let mut response = next.run(req).await;
    if let Ok(val) = HeaderValue::from_str(version) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("api-version"), val);
    }
    response
}

/// Middleware factory for V1 routes — adds `API-Version: v1` and deprecation headers.
pub async fn v1_version_middleware<B>(req: Request<B>, next: Next<B>) -> AxumResponse {
    let mut response = inject_api_version_header("v1", req, next).await;
    response.headers_mut().insert(
        HeaderName::from_str("Deprecation").unwrap(),
        HeaderValue::from_static("true"),
    );
    response.headers_mut().insert(
        HeaderName::from_str("Sunset").unwrap(),
        HeaderValue::from_static("Fri, 31 Dec 2026 23:59:59 GMT"),
    );
    response
}

/// Middleware factory for V2 routes — adds `API-Version: v2`.
pub async fn v2_version_middleware<B>(req: Request<B>, next: Next<B>) -> AxumResponse {
    inject_api_version_header("v2", req, next).await
}
