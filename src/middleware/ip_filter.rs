use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::task::{Context, Poll};

use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::{Layer, Service};

use crate::config::AllowedIps;

#[derive(Clone, Debug)]
pub struct IpFilterLayer {
    allowed_ips: AllowedIps,
    trusted_proxy_depth: usize,
}

impl IpFilterLayer {
    pub fn new(allowed_ips: AllowedIps, trusted_proxy_depth: usize) -> Self {
        Self {
            allowed_ips,
            trusted_proxy_depth,
        }
    }
}

impl<S> Layer<S> for IpFilterLayer {
    type Service = IpFilterService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        IpFilterService {
            inner,
            allowed_ips: self.allowed_ips.clone(),
            trusted_proxy_depth: self.trusted_proxy_depth,
        }
    }
}

#[derive(Clone, Debug)]
pub struct IpFilterService<S> {
    inner: S,
    allowed_ips: AllowedIps,
    trusted_proxy_depth: usize,
}

impl<S, B> Service<Request<B>> for IpFilterService<S>
where
    S: Service<Request<B>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = futures_util::future::BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let allowed_ips = self.allowed_ips.clone();
        let trusted_proxy_depth = self.trusted_proxy_depth;

        let client_ip = extract_client_ip(req.headers(), req.extensions(), trusted_proxy_depth);
        let allowed = is_allowed(client_ip, &allowed_ips);

        if !allowed {
            tracing::warn!(client_ip = ?client_ip, "blocked callback request from non-whitelisted IP");
            let response = StatusCode::FORBIDDEN.into_response();
            return Box::pin(async move { Ok(response) });
        }

        let mut inner = self.inner.clone();
        Box::pin(async move { inner.call(req).await })
    }
}

fn is_allowed(client_ip: Option<IpAddr>, allowed_ips: &AllowedIps) -> bool {
    match allowed_ips {
        AllowedIps::Any => true,
        AllowedIps::Cidrs(cidrs) => client_ip
            .map(|ip| cidrs.iter().any(|cidr| cidr.contains(&ip)))
            .unwrap_or(false),
    }
}

fn extract_client_ip(
    headers: &HeaderMap,
    extensions: &axum::http::Extensions,
    trusted_proxy_depth: usize,
) -> Option<IpAddr> {
    if let Some(ip) = extract_from_x_forwarded_for(headers, trusted_proxy_depth) {
        return Some(ip);
    }

    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip())
}

fn extract_from_x_forwarded_for(headers: &HeaderMap, trusted_proxy_depth: usize) -> Option<IpAddr> {
    let raw = headers.get("x-forwarded-for")?.to_str().ok()?;

    let chain: Vec<IpAddr> = raw
        .split(',')
        .map(str::trim)
        .filter_map(parse_ip_from_xff_entry)
        .collect();

    if chain.is_empty() || trusted_proxy_depth >= chain.len() {
        return None;
    }

    let index = chain.len().saturating_sub(1 + trusted_proxy_depth);
    chain.get(index).copied()
}

fn parse_ip_from_xff_entry(value: &str) -> Option<IpAddr> {
    if let Ok(ip) = IpAddr::from_str(value) {
        return Some(ip);
    }

    if let Ok(addr) = SocketAddr::from_str(value) {
        return Some(addr.ip());
    }

    None
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderValue, Request};
    use ipnet::IpNet;
    use tower::ServiceExt;
    use tower::service_fn;
    use tracing::Subscriber;
    use tracing_subscriber::layer::{Context as LayerContext, Layer};
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::Registry;

    #[test]
    fn xff_uses_client_ip_with_single_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.10, 198.51.100.7"),
        );

        let ip = extract_from_x_forwarded_for(&headers, 1);
        assert_eq!(ip, Some(IpAddr::from([203, 0, 113, 10])));
    }

    #[test]
    fn xff_returns_none_when_depth_exceeds_chain() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.10"),
        );

        let ip = extract_from_x_forwarded_for(&headers, 1);
        assert_eq!(ip, None);
    }

    #[test]
    fn cidr_allowlist_matches_ip() {
        let allowed = AllowedIps::Cidrs(vec![
            "203.0.113.0/24".parse::<IpNet>().expect("valid cidr"),
        ]);

        assert!(is_allowed(Some(IpAddr::from([203, 0, 113, 10])), &allowed));
        assert!(!is_allowed(Some(IpAddr::from([198, 51, 100, 10])), &allowed));
    }

    #[tokio::test]
    async fn allowed_ip_request_passes() {
        let layer = IpFilterLayer::new(
            AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]),
            1,
        );
        let service = layer.layer(service_fn(|_req: Request<Body>| async move {
            Ok::<Response, Infallible>(StatusCode::OK.into_response())
        }));

        let mut req = Request::builder()
            .uri("/callback/transaction")
            .body(Body::empty())
            .expect("request");
        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.55, 198.51.100.7"),
        );

        let res = service.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_whitelisted_ip_returns_forbidden() {
        let layer = IpFilterLayer::new(
            AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]),
            1,
        );
        let service = layer.layer(service_fn(|_req: Request<Body>| async move {
            Ok::<Response, Infallible>(StatusCode::OK.into_response())
        }));

        let mut req = Request::builder()
            .uri("/callback/transaction")
            .body(Body::empty())
            .expect("request");
        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.55, 198.51.100.7"),
        );

        let res = service.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn bypass_mode_allows_any_ip() {
        let layer = IpFilterLayer::new(AllowedIps::Any, 1);
        let service = layer.layer(service_fn(|_req: Request<Body>| async move {
            Ok::<Response, Infallible>(StatusCode::OK.into_response())
        }));

        let mut req = Request::builder()
            .uri("/callback/transaction")
            .body(Body::empty())
            .expect("request");
        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.55, 198.51.100.7"),
        );

        let res = service.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connect_info_is_used_when_xff_absent() {
        let layer = IpFilterLayer::new(
            AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]),
            1,
        );
        let service = layer.layer(service_fn(|_req: Request<Body>| async move {
            Ok::<Response, Infallible>(StatusCode::OK.into_response())
        }));

        let mut req = Request::builder()
            .uri("/callback/transaction")
            .body(Body::empty())
            .expect("request");
        req.extensions_mut().insert(ConnectInfo(SocketAddr::from((
            [203, 0, 113, 44],
            8080,
        ))));

        let res = service.oneshot(req).await.expect("response");
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn blocked_request_is_logged() {
        let captured = Arc::new(Mutex::new(Vec::<String>::new()));
        let subscriber = Registry::default().with(CaptureWarnLayer {
            events: Arc::clone(&captured),
        });
        let _guard = tracing::subscriber::set_default(subscriber);

        let layer = IpFilterLayer::new(
            AllowedIps::Cidrs(vec!["203.0.113.0/24".parse::<IpNet>().expect("valid cidr")]),
            1,
        );
        let service = layer.layer(service_fn(|_req: Request<Body>| async move {
            Ok::<Response, Infallible>(StatusCode::OK.into_response())
        }));

        let mut req = Request::builder()
            .uri("/callback/transaction")
            .body(Body::empty())
            .expect("request");
        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.55, 198.51.100.7"),
        );

        let _ = service.oneshot(req).await.expect("response");

        let events = captured.lock().expect("poisoned mutex");
        assert!(
            events
                .iter()
                .any(|event| event.contains("blocked callback request from non-whitelisted IP")),
            "expected blocked IP log event"
        );
    }

    #[derive(Clone)]
    struct CaptureWarnLayer {
        events: Arc<Mutex<Vec<String>>>,
    }

    impl<S> Layer<S> for CaptureWarnLayer
    where
        S: Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: LayerContext<'_, S>) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }

            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);
            let message = visitor.message.unwrap_or_else(|| event.metadata().name().to_string());
            self.events
                .lock()
                .expect("poisoned mutex")
                .push(message);
        }
    }

    #[derive(Default)]
    struct MessageVisitor {
        message: Option<String>,
    }

    impl tracing::field::Visit for MessageVisitor {
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if field.name() == "message" {
                self.message = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.message = Some(format!("{value:?}"));
            }
        }
    }
}
