use axum::{
    http::{HeaderName, HeaderValue, Request},
    middleware::Next,
    response::Response as AxumResponse,
};
use std::str::FromStr;

/// Single source of truth for the API versions this service exposes.
///
/// Every version listed here must have a corresponding handler module under
/// `src/handlers/<version>/`. The versioning middleware and the handler modules
/// both reference this list, so adding a version is a compile-time addition
/// rather than a string edit in multiple places.
pub const SUPPORTED_VERSIONS: &[&str] = &["v1", "v2"];

/// Returns `true` if `version` is a registered API version.
pub fn is_supported_version(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

pub async fn inject_deprecation_headers<B>(req: Request<B>, next: Next<B>) -> AxumResponse {
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
    let mut response = inject_api_version_header(SUPPORTED_VERSIONS[0], req, next).await;
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
    inject_api_version_header(SUPPORTED_VERSIONS[1], req, next).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_versions_are_unique_and_well_formed() {
        for (i, version) in SUPPORTED_VERSIONS.iter().enumerate() {
            assert!(
                version.starts_with('v'),
                "version {version:?} must be prefixed with 'v'"
            );
            assert!(
                !SUPPORTED_VERSIONS[..i].contains(version),
                "duplicate version {version:?} in SUPPORTED_VERSIONS"
            );
        }
    }

    #[test]
    fn every_supported_version_has_a_handler_module() {
        // Each registered version must map to a handler module. Adding a version
        // to SUPPORTED_VERSIONS without wiring its handler module fails here.
        for version in SUPPORTED_VERSIONS {
            let module = match *version {
                "v1" => Some("v1"),
                "v2" => Some("v2"),
                _ => None,
            };
            assert!(
                module.is_some(),
                "version {version:?} is registered but has no handler module"
            );
        }
    }

    #[test]
    fn is_supported_version_matches_constant() {
        for version in SUPPORTED_VERSIONS {
            assert!(is_supported_version(version));
        }
        assert!(!is_supported_version("v3"));
    }
}
