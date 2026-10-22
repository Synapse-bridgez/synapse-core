//! Versioning + deprecation policy for the swap API surface (#1507).
//!
//! Scheme: **path prefix** `/swap/v{N}/...` (tenant) and `/admin/swap/v{N}/...`
//! (operator). All swap routes are mounted through [`swap_router`] /
//! [`admin_swap_router`], so a new endpoint cannot escape the policy. Deprecation
//! metadata lives in [`VERSIONS`] next to the routing, not in prose.
//!
//! * Unversioned (`/swap/analytics`) and unsupported (`/swap/v9/...`) calls get a
//!   structured error, never a silent fallback.
//! * Streams (WebSocket/SSE) negotiate with [`negotiate_stream_version`]
//!   (`?version=` query, else `Sec-WebSocket-Protocol: swap.v1`).
//!
//! See `docs/swap-api-versioning.md` for compatibility rules.

use crate::handlers::swap as h;
use crate::ApiState;
use axum::{
    http::{header, HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionStatus {
    Supported,
    /// Still served, with `Deprecation`/`Sunset` headers until `sunset`.
    Deprecated {
        sunset: &'static str,
    },
}

pub struct VersionInfo {
    pub version: u32,
    pub status: VersionStatus,
}

/// The deprecation registry: every version the swap API serves.
pub const VERSIONS: &[VersionInfo] = &[VersionInfo {
    version: 1,
    status: VersionStatus::Supported,
}];

pub fn lookup(version: u32) -> Option<&'static VersionInfo> {
    VERSIONS.iter().find(|v| v.version == version)
}

/// Parses `v1` / `1` into a version number.
pub fn parse_version(s: &str) -> Option<u32> {
    s.trim().trim_start_matches(['v', 'V']).parse().ok()
}

/// Version negotiation for WebSocket/SSE endpoints. Returns the requested
/// version if supported; `Err` carries the supported list for the error body.
pub fn negotiate_stream_version(
    query_version: Option<&str>,
    headers: &HeaderMap,
) -> Result<u32, Vec<u32>> {
    let requested = query_version.and_then(parse_version).or_else(|| {
        headers
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| {
                v.split(',')
                    .find_map(|p| p.trim().strip_prefix("swap.").and_then(parse_version))
            })
    });
    match requested {
        Some(v) if lookup(v).is_some() => Ok(v),
        _ => Err(VERSIONS.iter().map(|v| v.version).collect()),
    }
}

fn error(status: StatusCode, code: &str, msg: String) -> Response {
    let supported: Vec<String> = VERSIONS.iter().map(|v| format!("v{}", v.version)).collect();
    (
        status,
        Json(serde_json::json!({ "error": code, "message": msg, "supported_versions": supported })),
    )
        .into_response()
}

/// First path segment as seen by the swap router (the `/swap` or
/// `/admin/swap` mount prefix has already been stripped by `nest`).
fn path_version(path: &str) -> Option<&str> {
    path.trim_start_matches('/').split('/').next()
}

/// Enforces the policy: rejects unversioned/unsupported calls and stamps
/// `API-Version` plus `Deprecation`/`Sunset` on deprecated versions.
pub async fn swap_version_middleware<B>(req: Request<B>, next: Next<B>) -> Response {
    let seg = path_version(req.uri().path()).map(str::to_string);
    let Some(seg) = seg else {
        return next.run(req).await;
    };
    let looks_versioned =
        seg.starts_with('v') && seg[1..].chars().all(|c| c.is_ascii_digit()) && seg.len() > 1;
    if !looks_versioned {
        return error(
            StatusCode::BAD_REQUEST,
            "version_required",
            format!("swap endpoints require a version prefix, e.g. /swap/v1/{seg}"),
        );
    }
    let Some(info) = parse_version(&seg).and_then(lookup) else {
        return error(
            StatusCode::NOT_FOUND,
            "unsupported_version",
            format!("swap API version '{seg}' is not supported"),
        );
    };
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&format!("v{}", info.version)) {
        h.insert(HeaderName::from_static("api-version"), v);
    }
    if let VersionStatus::Deprecated { sunset } = info.status {
        if let Ok(v) = HeaderValue::from_str(sunset) {
            h.insert(
                HeaderName::from_static("deprecation"),
                HeaderValue::from_static("true"),
            );
            h.insert(HeaderName::from_static("sunset"), v);
        }
    }
    res
}

/// Tenant-facing swap routes. Auth is per-handler via `TenantContext`.
pub fn swap_router() -> Router<ApiState> {
    Router::new()
        .nest(
            "/v1",
            Router::new().route("/analytics", get(h::tenant_analytics)),
        )
        .fallback(|| async {
            error(
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown swap endpoint".into(),
            )
        })
        .layer(middleware::from_fn(swap_version_middleware))
}

/// Operator routes; the caller must apply `admin_auth`.
pub fn admin_swap_router() -> Router<ApiState> {
    Router::new()
        .nest(
            "/v1",
            Router::new()
                .route("/surveillance/findings", get(h::list_findings))
                .route(
                    "/surveillance/findings/:id/status",
                    post(h::update_finding_status),
                )
                .route("/surveillance/findings/:id/audit", get(h::finding_audit))
                .route("/market-data/quotes", get(h::archived_quotes))
                .route("/market-data/prices", get(h::archived_prices)),
        )
        .fallback(|| async {
            error(
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown swap endpoint".into(),
            )
        })
        .layer(middleware::from_fn(swap_version_middleware))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new().nest(
            "/swap",
            Router::new()
                .nest("/v1", Router::new().route("/ping", get(|| async { "ok" })))
                .fallback(|| async {
                    error(
                        StatusCode::NOT_FOUND,
                        "not_found",
                        "unknown swap endpoint".into(),
                    )
                })
                .layer(middleware::from_fn(swap_version_middleware)),
        )
    }

    async fn status(uri: &str) -> (StatusCode, Option<String>) {
        let res = app()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        (
            res.status(),
            res.headers()
                .get("api-version")
                .map(|v| v.to_str().unwrap().to_string()),
        )
    }

    #[tokio::test]
    async fn versioned_route_is_served_with_api_version_header() {
        assert_eq!(
            status("/swap/v1/ping").await,
            (StatusCode::OK, Some("v1".into()))
        );
    }

    #[tokio::test]
    async fn unversioned_and_unsupported_are_explicit_errors() {
        assert_eq!(status("/swap/ping").await.0, StatusCode::BAD_REQUEST);
        assert_eq!(status("/swap/v9/ping").await.0, StatusCode::NOT_FOUND);
    }

    #[test]
    fn stream_negotiation_prefers_query_then_subprotocol() {
        let mut h = HeaderMap::new();
        h.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            "swap.v1, json".parse().unwrap(),
        );
        assert_eq!(negotiate_stream_version(None, &h), Ok(1));
        assert_eq!(
            negotiate_stream_version(Some("v1"), &HeaderMap::new()),
            Ok(1)
        );
        assert_eq!(negotiate_stream_version(Some("v7"), &h), Err(vec![1]));
        assert!(negotiate_stream_version(None, &HeaderMap::new()).is_err());
    }

    #[test]
    fn deprecated_status_carries_sunset_date() {
        let s = VersionStatus::Deprecated {
            sunset: "Fri, 31 Dec 2027 23:59:59 GMT",
        };
        assert!(matches!(s, VersionStatus::Deprecated { sunset } if sunset.contains("2027")));
    }
}
