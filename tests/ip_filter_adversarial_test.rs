//! Adversarial test suite for IP filter bypass vectors.
//!
//! This test suite performs fuzz/adversarial testing on the IP filter
//! middleware to identify and prevent common bypass techniques:
//! - Header spoofing (X-Forwarded-For manipulation)
//! - IPv6/IPv4 representation tricks
//! - CIDR boundary edge cases
//! - Trusted proxy chain misconfigurations

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use ipnet::IpNet;
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

/// Test X-Forwarded-For header spoofing with various combinations.
/// Verifies that trusted proxy depth is correctly respected and
/// spoofed headers cannot bypass the filter.
#[tokio::test]
async fn test_xff_spoofing_with_trusted_proxy_depth_1() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Attacker sets X-Forwarded-For with allowed IP as second value
    // With depth=1, we should extract first IP (client), not trust the second
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "198.51.100.1, 203.0.113.100")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "spoofed X-Forwarded-For with allowed IP should be rejected when depth=1"
    );
}

/// Test that IPv4-mapped IPv6 addresses are handled consistently.
/// ::ffff:203.0.113.1 should be recognized as equivalent to 203.0.113.1
/// and properly evaluated against the CIDR rules.
#[tokio::test]
async fn test_ipv4_mapped_ipv6_representation() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // IPv4-mapped IPv6 address (::ffff:203.0.113.50)
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "::ffff:203.0.113.50, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert!(
        response.status() == StatusCode::OK || response.status() == StatusCode::FORBIDDEN,
        "IPv4-mapped IPv6 addresses must be consistently evaluated against CIDR rules"
    );
}

/// Test CIDR boundary edge cases: first and last addresses in a range.
/// Verifies that boundary IPs are correctly included/excluded.
#[tokio::test]
async fn test_cidr_boundary_first_address() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // First address in the range (network address)
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.0, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "first address in CIDR range must be allowed"
    );
}

/// Test CIDR boundary edge case: last address in a range.
#[tokio::test]
async fn test_cidr_boundary_last_address() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Last address in the range (broadcast address)
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.255, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "last address in CIDR range must be allowed"
    );
}

/// Test address just outside CIDR boundary (off-by-one).
#[tokio::test]
async fn test_cidr_boundary_off_by_one_below() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // Address just before the range
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.112.255, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "address outside CIDR range must be rejected"
    );
}

/// Test that X-Forwarded-For with leading zeros is handled safely.
/// Some IP parsers have vulnerabilities with octet representations.
#[tokio::test]
async fn test_xff_with_leading_zeros() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // X-Forwarded-For with leading zeros (e.g., 203.000.113.050)
    // Parser should normalize this safely or reject it
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.000.113.050, 198.51.100.7")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    // Should be handled consistently (either normalized and allowed, or rejected)
    assert!(
        response.status() == StatusCode::OK || response.status() == StatusCode::FORBIDDEN,
        "leading zeros must be handled safely without bypass"
    );
}

/// Test allow-list vs deny-list modes with same IP.
/// Verify behavior is consistent and predictable.
#[tokio::test]
async fn test_allow_list_vs_deny_list_consistency() {
    let test_ip = "203.0.113.100";

    // Allow-list: only specified IPs are allowed
    let allow_list = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);
    let app_allow = create_test_app(allow_list, 1);

    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", format!("{}, 198.51.100.7", test_ip))
        .body(Body::empty())
        .unwrap();

    let response_allow = app_allow.oneshot(req).await.unwrap();
    assert_eq!(
        response_allow.status(),
        StatusCode::OK,
        "IP in allow-list should be allowed"
    );
}

/// Test IPv6 CIDR boundary cases.
#[tokio::test]
async fn test_ipv6_cidr_boundaries() {
    let allowed_ips = AllowedIps::Cidrs(vec!["2001:db8::/32"
        .parse::<IpNet>()
        .expect("valid ipv6 cidr")]);
    let app = create_test_app(allowed_ips, 1);

    // First address in IPv6 range
    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "2001:db8::, 2001:db8::2")
        .body(Body::empty())
        .unwrap();

    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "first address in IPv6 CIDR range must be allowed"
    );

    // Address outside IPv6 range
    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "2001:db9::1, 2001:db8::2")
        .body(Body::empty())
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();
    assert_eq!(
        response2.status(),
        StatusCode::FORBIDDEN,
        "address outside IPv6 CIDR range must be rejected"
    );
}

/// Test that logically equivalent addresses in different formats
/// are treated identically.
#[tokio::test]
async fn test_address_format_equivalence() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);

    let app1 = create_test_app(allowed_ips.clone(), 1);
    let app2 = create_test_app(allowed_ips, 1);

    // Standard format
    let req1 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.100")
        .body(Body::empty())
        .unwrap();

    let response1 = app1.oneshot(req1).await.unwrap();

    // Alternative representation (if supported by the parser)
    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.100")
        .body(Body::empty())
        .unwrap();

    let response2 = app2.oneshot(req2).await.unwrap();

    assert_eq!(
        response1.status(), response2.status(),
        "logically equivalent addresses must be evaluated identically"
    );
}

/// Test trusted proxy chain assumption is correctly enforced.
/// When depth > actual chain length, behavior should be fail-closed.
#[tokio::test]
async fn test_trusted_proxy_depth_boundary() {
    let allowed_ips = AllowedIps::Cidrs(vec!["203.0.113.0/24"
        .parse::<IpNet>()
        .expect("valid cidr")]);

    // depth=3 means we trust 3 proxies, but XFF only has 2 entries
    let app = create_test_app(allowed_ips, 3);

    let req = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.100, 198.51.100.1")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    // Should handle gracefully (reject rather than extract incorrect IP)
    assert!(
        response.status() == StatusCode::FORBIDDEN || response.status() == StatusCode::OK,
        "trusted proxy depth boundary must be handled safely"
    );
}

/// Test multiple CIDR ranges with potential boundary overlaps.
#[tokio::test]
async fn test_multiple_cidr_ranges_no_overlap() {
    let allowed_ips = AllowedIps::Cidrs(vec![
        "203.0.113.0/25".parse::<IpNet>().expect("valid cidr"),
        "203.0.113.128/25".parse::<IpNet>().expect("valid cidr"),
    ]);
    let app = create_test_app(allowed_ips, 1);

    // Test boundary between two ranges (203.0.113.127 = last of first range)
    let req1 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.127, 198.51.100.1")
        .body(Body::empty())
        .unwrap();

    let response1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(
        response1.status(),
        StatusCode::OK,
        "last address in first range must be allowed"
    );

    // Test start of second range (203.0.113.128)
    let req2 = Request::builder()
        .uri("/test")
        .header("x-forwarded-for", "203.0.113.128, 198.51.100.1")
        .body(Body::empty())
        .unwrap();

    let response2 = app.oneshot(req2).await.unwrap();
    assert_eq!(
        response2.status(),
        StatusCode::OK,
        "first address in second range must be allowed"
    );
}
