use axum::{
    http::{HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response as AxumResponse,
};
use std::str::FromStr;

/// Minimum notice period (in days) that a surface must be marked deprecated
/// before it may be removed. See `docs/deprecation-policy.md`.
pub const MIN_DEPRECATION_NOTICE_DAYS: u64 = 180;

/// A reusable description of a deprecation, used to emit the standard
/// `Deprecation` / `Sunset` HTTP headers (IETF draft convention) so future
/// deprecations can adopt the mechanism without new infrastructure work.
#[derive(Debug, Clone, Copy)]
pub struct Deprecation {
    /// The `Sunset` date, formatted as an IMF-fixdate (e.g. `Fri, 31 Dec 2026 23:59:59 GMT`).
    pub sunset: &'static str,
    /// Optional link to the deprecation policy / migration documentation.
    pub link: Option<&'static str>,
}

impl Deprecation {
    /// Construct a deprecation with the given `Sunset` date and no documentation link.
    pub const fn new(sunset: &'static str) -> Self {
        Self { sunset, link: None }
    }

    /// Attach a documentation link describing the deprecation and migration path.
    pub const fn with_link(mut self, link: &'static str) -> Self {
        self.link = Some(link);
        self
    }

    /// Emit the `Deprecation` and `Sunset` headers onto a response.
    ///
    /// The `Deprecation` header is set to `true` and the `Sunset` header to the
    /// configured date. When a link is present it is advertised via the standard
    /// `Link` header with `rel="deprecation"` so clients can discover the policy.
    pub fn apply(&self, response: &mut AxumResponse) {
        let headers = response.headers_mut();
        headers.insert(
            HeaderName::from_static("deprecation"),
            HeaderValue::from_static("true"),
        );
        if let Ok(val) = HeaderValue::from_str(self.sunset) {
            headers.insert(HeaderName::from_static("sunset"), val);
        }
        if let Some(link) = self.link {
            if let Ok(val) = HeaderValue::from_str(&format!("<{link}>; rel=\"deprecation\"")) {
                headers.insert(HeaderName::from_static("link"), val);
            }
        }
    }
}

/// The deprecation applied to the v1 API surface.
pub const V1_DEPRECATION: Deprecation =
    Deprecation::new("Fri, 31 Dec 2026 23:59:59 GMT");

pub async fn inject_deprecation_headers<B>(req: Request<B>, next: Next<B>) -> AxumResponse {
    let mut response = next.run(req).await;
    V1_DEPRECATION.apply(&mut response);
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
    V1_DEPRECATION.apply(&mut response);
    response
}

/// Middleware factory for V2 routes — adds `API-Version: v2`.
pub async fn v2_version_middleware<B>(req: Request<B>, next: Next<B>) -> AxumResponse {
    inject_api_version_header("v2", req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route("/v1", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(v1_version_middleware))
    }

    #[tokio::test]
    async fn v1_emits_deprecation_and_sunset_headers() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("deprecation").unwrap(), "true");
        assert_eq!(
            response.headers().get("sunset").unwrap(),
            V1_DEPRECATION.sunset
        );
        assert_eq!(response.headers().get("api-version").unwrap(), "v1");
    }

    #[test]
    fn deprecation_apply_sets_link_when_present() {
        let mut response = AxumResponse::new(Body::empty());
        Deprecation::new("Fri, 31 Dec 2026 23:59:59 GMT")
            .with_link("https://example.com/deprecation-policy")
            .apply(&mut response);

        assert_eq!(response.headers().get("deprecation").unwrap(), "true");
        assert_eq!(
            response.headers().get("sunset").unwrap(),
            "Fri, 31 Dec 2026 23:59:59 GMT"
        );
        assert_eq!(
            response.headers().get("link").unwrap(),
            "<https://example.com/deprecation-policy>; rel=\"deprecation\""
        );
    }
}
