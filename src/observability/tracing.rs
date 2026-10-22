//! Correlation ids and span conventions for the swap lifecycle.
//!
//! The OTLP tracer itself is initialised by [`crate::telemetry::init_tracer`]
//! (and degrades gracefully with no collector). This module adds the swap-domain
//! pieces: a propagated correlation id, span attribute naming, and sampling
//! config. See `docs/swap-tracing.md`.

use std::future::Future;

pub const CORRELATION_HEADER: &str = "x-correlation-id";
const MAX_LEN: usize = 128;

tokio::task_local! {
    static CORRELATION_ID: String;
}

/// Accept a caller-supplied id when it is safe, otherwise generate one. A
/// missing or malformed header never rejects the request.
pub fn resolve_correlation_id(header: Option<&str>) -> String {
    match header.map(str::trim) {
        Some(h)
            if !h.is_empty()
                && h.len() <= MAX_LEN
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            h.to_string()
        }
        _ => uuid::Uuid::new_v4().to_string(),
    }
}

/// Current correlation id, if running inside [`with_correlation_id`].
pub fn current_correlation_id() -> Option<String> {
    CORRELATION_ID.try_with(|c| c.clone()).ok()
}

/// Run `fut` with `id` as the ambient correlation id.
pub async fn with_correlation_id<F: Future>(id: String, fut: F) -> F::Output {
    CORRELATION_ID.scope(id, fut).await
}

/// `tokio::spawn` that carries the correlation id across the task boundary.
pub fn spawn_correlated<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current_correlation_id() {
        Some(id) => tokio::spawn(CORRELATION_ID.scope(id, fut)),
        None => tokio::spawn(fut),
    }
}

/// Span attribute keys (`swap.*`). Values must never be secrets or raw PII.
pub mod attr {
    pub const CORRELATION_ID: &str = "swap.correlation_id";
    pub const TENANT_ID: &str = "swap.tenant_id";
    pub const ASSET_PAIR: &str = "swap.asset_pair";
    pub const ORDER_ID: &str = "swap.order_id";
    pub const LIQUIDITY_PROVIDER: &str = "swap.liquidity_provider";
}

/// Swap lifecycle stage span names.
pub const STAGES: [&str; 6] = [
    "swap.ingress",
    "swap.quote",
    "swap.route",
    "swap.execute",
    "swap.settle",
    "swap.webhook",
];

/// Open a swap span carrying the standard attributes.
pub fn swap_span(
    stage: &'static str,
    tenant_id: &str,
    asset_pair: &str,
    order_id: &str,
    liquidity_provider: &str,
) -> ::tracing::Span {
    let cid = current_correlation_id().unwrap_or_default();
    ::tracing::info_span!(
        "swap",
        stage,
        swap.correlation_id = %cid,
        swap.tenant_id = tenant_id,
        swap.asset_pair = asset_pair,
        swap.order_id = order_id,
        swap.liquidity_provider = liquidity_provider,
    )
}

/// Trace sampling config: low by default; the ratio-based sampler keeps the
/// decision consistent within a trace. `SWAP_TRACE_SAMPLE_RATIO` (0.0-1.0).
pub fn sample_ratio_from_env() -> f64 {
    std::env::var("SWAP_TRACE_SAMPLE_RATIO")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|r| (0.0..=1.0).contains(r))
        .unwrap_or(0.01)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_and_replaces_invalid_ids() {
        assert_eq!(resolve_correlation_id(Some("abc-123_X")), "abc-123_X");
        assert_ne!(resolve_correlation_id(Some("bad id\n")), "bad id\n");
        assert_eq!(resolve_correlation_id(None).len(), 36);
        assert_eq!(resolve_correlation_id(Some(&"a".repeat(200))).len(), 36);
    }

    #[tokio::test]
    async fn id_propagates_through_spawn() {
        let id = "corr-1".to_string();
        let got = with_correlation_id(id.clone(), async {
            spawn_correlated(async { current_correlation_id() })
                .await
                .unwrap()
        })
        .await;
        assert_eq!(got, Some(id));
        assert_eq!(current_correlation_id(), None);
    }

    #[test]
    fn default_sampling_is_low() {
        std::env::remove_var("SWAP_TRACE_SAMPLE_RATIO");
        assert!(sample_ratio_from_env() <= 0.05);
    }
}
