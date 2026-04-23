use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use ipnet::IpNet;
use std::net::SocketAddr;
use synapse_core::config::AllowedIps;
use synapse_core::middleware::ip_filter::IpFilterLayer;
use tower::ServiceExt;

async fn test_handler() -> Response {
    StatusCode::OK.into_response()
}

fn create_test_app(allowed_ips: AllowedIps, trusted_proxy_depth: usize) -> Router {
    Router::new()
        .route("/test", get(test_handler))
        .layer(IpFilterLayer::new(allowed_ips, trusted_proxy_depth))
}

#[tokio::test]
async fn test_ip_filter_allowed_ip() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Request from allowed IP via X-Forwarded-For
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.55, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ip_filter_blocked_ip() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Request from blocked IP via X-Forwarded-For
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "198.51.100.55, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ip_filter_xff_header() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Test with X-Forwarded-For chain: client, proxy
    // With trusted_proxy_depth=1, we extract the client IP (first in chain)
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.10, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ip_filter_xff_trusted_proxy_depth() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);

    // With trusted_proxy_depth=2, we trust the last 2 proxies
    let app = create_test_app(allowed_ips, 2);

    // Chain: client -> proxy1 -> proxy2 -> us
    // X-Forwarded-For: client, proxy1, proxy2
    // With depth=2, we extract the IP at position: len - 1 - depth = 3 - 1 - 2 = 0 (client)
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.20, 192.168.1.1, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ip_filter_cidr_range() {
    // Test multiple CIDR ranges
    let allowed_ips = AllowedIps::Cidrs(vec![
        "203.0.113.0/24".parse::<IpNet>().expect("valid cidr"),
        "198.51.100.0/24".parse::<IpNet>().expect("valid cidr"),
        "192.0.2.0/24".parse::<IpNet>().expect("valid cidr"),
    ]);
    let app = create_test_app(allowed_ips, 1);

    // Test IP from first range
    let req1 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.100, 10.0.0.1")
        .body(Body::empty())
        .unwrap();
    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    // Test IP from second range
    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "198.51.100.50, 10.0.0.1")
        .body(Body::empty())
        .unwrap();
    let response2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);

    // Test IP from third range
    let req3 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "192.0.2.75, 10.0.0.1")
        .body(Body::empty())
        .unwrap();
    let response3 = app.clone().oneshot(req3).await.unwrap();
    assert_eq!(response3.status(), StatusCode::OK);

    // Test IP outside all ranges
    let req4 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "10.0.0.100, 10.0.0.1")
        .body(Body::empty())
        .unwrap();
    let response4 = app.oneshot(req4).await.unwrap();
    assert_eq!(response4.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ip_filter_bypass_mode() {
    // AllowedIps::Any allows all IPs
    let allowed_ips = AllowedIps::Any;
    let app = create_test_app(allowed_ips, 1);

    // Test with any IP - should all pass
    let req1 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "198.51.100.55, 198.51.100.7")
        .body(Body::empty())
        .unwrap();
    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "10.0.0.1, 192.168.1.1")
        .body(Body::empty())
        .unwrap();
    let response2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::OK);

    let req3 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "1.2.3.4, 5.6.7.8")
        .body(Body::empty())
        .unwrap();
    let response3 = app.oneshot(req3).await.unwrap();
    assert_eq!(response3.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ip_filter_connect_info_fallback() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Request without X-Forwarded-For, using ConnectInfo
    let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

    // Add ConnectInfo extension with allowed IP
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([203, 0, 113, 44], 8080))));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_ip_filter_connect_info_blocked() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Request without X-Forwarded-For, using ConnectInfo with blocked IP
    let mut req = Request::builder().uri("/test").body(Body::empty()).unwrap();

    // Add ConnectInfo extension with blocked IP
    req.extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([198, 51, 100, 44], 8080))));

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ip_filter_ipv6_support() {
    // Test IPv6 CIDR range
    let allowed_ips = AllowedIps::Cidrs(vec!["2001:db8::/32"
        .parse::<IpNet>()
        .expect("valid ipv6 cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Test allowed IPv6
    let req1 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "2001:db8::1, 2001:db8::2")
        .body(Body::empty())
        .unwrap();
    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(response1.status(), StatusCode::OK);

    // Test blocked IPv6
    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "2001:db9::1, 2001:db8::2")
        .body(Body::empty())
        .unwrap();
    let response2 = app.oneshot(req2).await.unwrap();
    assert_eq!(response2.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ip_filter_no_xff_no_connect_info() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Request without X-Forwarded-For and without ConnectInfo
    let req = Request::builder().uri("/test").body(Body::empty()).unwrap();

    let response = app.oneshot(req).await.unwrap();
    // Should be blocked because no IP can be extracted
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_ip_filter_malformed_xff() {
    let allowed_ips =
        AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Test with malformed X-Forwarded-For
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "not-an-ip, also-not-an-ip")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    // Should be blocked because no valid IP can be extracted
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
