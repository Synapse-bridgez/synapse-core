//! Multi-hop route discovery and scoring.
//!
//! The router builds a directed graph of tradable pairs from every
//! registered [`LiquidityProvider`] (one edge per provider per pair), then:
//!
//! 1. **Discovers** simple paths (no asset visited twice) from the source to
//!    the destination asset, shortest first, up to `max_hops` legs and at
//!    most `max_candidates` paths. Enumeration order is fully deterministic
//!    (assets and providers are visited in sorted order).
//! 2. **Prices** each candidate by asking each hop's provider for a leg
//!    quote, feeding each leg's output into the next. Candidates with an
//!    unpriceable leg are dropped.
//! 3. **Scores** each priced route as its expected output minus a per-extra-
//!    hop penalty (`hop_penalty_bps`), which stands in for the extra
//!    execution risk and latency of every additional leg.
//! 4. **Selects** the highest score; ties go to fewer hops, then to the
//!    lexicographically smallest route key, so the same inputs always pick
//!    the same route.
//!
//! The chosen [`Route`] is returned in the quote and executed leg by leg.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::swap::asset::{Asset, AssetRegistry};
use crate::swap::decimal::{Decimal, BPS_DENOMINATOR};
use crate::swap::liquidity::LiquidityProvider;
use crate::swap::rate_conversion::implied_rate;
use crate::swap::rounding::{Purpose, RoundingPolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterConfig {
    /// Maximum legs in a route.
    pub max_hops: usize,
    /// Maximum candidate paths priced per quote.
    pub max_candidates: usize,
    /// Score penalty, in bps of output, for each leg beyond the first.
    pub hop_penalty_bps: u32,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            max_hops: 3,
            max_candidates: 32,
            hop_penalty_bps: 5,
        }
    }
}

/// One leg of a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteHop {
    pub provider: String,
    pub from: Asset,
    pub to: Asset,
    /// Expected amount sold on this leg (quote time).
    pub amount_in: Decimal,
    /// Expected amount received on this leg (quote time).
    pub expected_out: Decimal,
    /// Fee charged on this leg, in `from` units.
    pub fee: Decimal,
    /// Effective rate of this leg after fees (display only).
    pub rate: Decimal,
}

/// A priced path from source to destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub hops: Vec<RouteHop>,
    /// Asset codes along the path, source first.
    pub path: Vec<String>,
    pub amount_in: Decimal,
    pub expected_out: Decimal,
    /// Selection score (expected output after hop penalty), in destination
    /// precision.
    pub score: Decimal,
}

impl Route {
    /// Stable identifier for the route, e.g. `USDC>XLM@lp-a|XLM>EURC@lp-b`.
    pub fn key(&self) -> String {
        self.hops
            .iter()
            .map(|h| format!("{}>{}@{}", h.from.code, h.to.code, h.provider))
            .collect::<Vec<_>>()
            .join("|")
    }

    pub fn hop_count(&self) -> usize {
        self.hops.len()
    }
}

/// The router's choice plus how much of the search space it looked at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteSelection {
    pub route: Route,
    pub candidates_considered: usize,
    pub candidates_priced: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoutingError {
    #[error("unknown asset {0}")]
    UnknownAsset(String),
    #[error("source and destination asset are the same")]
    SameAsset,
    #[error("amount must be positive")]
    NonPositiveAmount,
    #[error("no path from {from} to {to} within {max_hops} hops")]
    NoPath {
        from: String,
        to: String,
        max_hops: usize,
    },
    #[error("no route from {from} to {to} could be priced ({candidates} candidates tried)")]
    NoPricedRoute {
        from: String,
        to: String,
        candidates: usize,
    },
}

#[derive(Debug, Clone)]
struct Edge {
    to: String,
    provider: usize,
}

/// Directed pair graph across all providers. Built per quote so provider
/// pair lists are always current.
pub struct LiquidityGraph {
    edges: BTreeMap<String, Vec<Edge>>,
}

impl LiquidityGraph {
    pub async fn build(providers: &[Arc<dyn LiquidityProvider>], assets: &AssetRegistry) -> Self {
        let mut set: BTreeSet<(String, String, String, usize)> = BTreeSet::new();
        for (idx, p) in providers.iter().enumerate() {
            match p.pairs().await {
                Ok(pairs) => {
                    for pair in pairs {
                        if pair.from == pair.to
                            || assets.get(&pair.from).is_none()
                            || assets.get(&pair.to).is_none()
                        {
                            continue;
                        }
                        set.insert((pair.from, pair.to, p.id().to_string(), idx));
                    }
                }
                Err(e) => {
                    tracing::warn!(provider = p.id(), error = %e, "liquidity provider pair discovery failed; skipping");
                }
            }
        }
        let mut edges: BTreeMap<String, Vec<Edge>> = BTreeMap::new();
        for (from, to, _provider_id, provider) in set {
            edges.entry(from).or_default().push(Edge { to, provider });
        }
        Self { edges }
    }

    /// Simple paths `from -> to` as lists of (from, to, provider index),
    /// shortest first, deterministic order, capped at `limit`.
    fn paths(
        &self,
        from: &str,
        to: &str,
        max_hops: usize,
        limit: usize,
    ) -> Vec<Vec<(String, String, usize)>> {
        let mut out = Vec::new();
        for depth in 1..=max_hops {
            let mut stack = Vec::new();
            let mut visited = BTreeSet::from([from.to_string()]);
            self.dfs(from, to, depth, &mut visited, &mut stack, &mut out, limit);
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn dfs(
        &self,
        at: &str,
        target: &str,
        remaining: usize,
        visited: &mut BTreeSet<String>,
        stack: &mut Vec<(String, String, usize)>,
        out: &mut Vec<Vec<(String, String, usize)>>,
        limit: usize,
    ) {
        if out.len() >= limit {
            return;
        }
        let Some(edges) = self.edges.get(at) else {
            return;
        };
        for e in edges {
            if out.len() >= limit {
                return;
            }
            if remaining == 1 {
                if e.to == target {
                    stack.push((at.to_string(), e.to.clone(), e.provider));
                    out.push(stack.clone());
                    stack.pop();
                }
                continue;
            }
            if e.to == target || visited.contains(&e.to) {
                continue;
            }
            visited.insert(e.to.clone());
            stack.push((at.to_string(), e.to.clone(), e.provider));
            self.dfs(&e.to, target, remaining - 1, visited, stack, out, limit);
            stack.pop();
            visited.remove(&e.to);
        }
    }
}

pub struct Router {
    pub config: RouterConfig,
    pub policy: RoundingPolicy,
}

impl Router {
    pub fn new(config: RouterConfig, policy: RoundingPolicy) -> Self {
        Self { config, policy }
    }

    /// Find and price the best route for selling `amount_in` of `from` for
    /// `to`. `amount_in` must already be in `from`'s precision.
    pub async fn best_route(
        &self,
        providers: &[Arc<dyn LiquidityProvider>],
        assets: &AssetRegistry,
        from: &str,
        to: &str,
        amount_in: &Decimal,
    ) -> Result<RouteSelection, RoutingError> {
        let src = assets
            .get(from)
            .ok_or_else(|| RoutingError::UnknownAsset(from.to_string()))?;
        let dst = assets
            .get(to)
            .ok_or_else(|| RoutingError::UnknownAsset(to.to_string()))?;
        if src.code == dst.code {
            return Err(RoutingError::SameAsset);
        }
        if !amount_in.is_positive() {
            return Err(RoutingError::NonPositiveAmount);
        }

        let graph = LiquidityGraph::build(providers, assets).await;
        let candidates = graph.paths(from, to, self.config.max_hops, self.config.max_candidates);
        if candidates.is_empty() {
            return Err(RoutingError::NoPath {
                from: from.to_string(),
                to: to.to_string(),
                max_hops: self.config.max_hops,
            });
        }

        let mut best: Option<Route> = None;
        let mut priced = 0;
        for path in &candidates {
            let Some(route) = self
                .price_path(providers, assets, path, amount_in, dst)
                .await
            else {
                continue;
            };
            priced += 1;
            if best.as_ref().is_none_or(|b| Self::better(&route, b)) {
                best = Some(route);
            }
        }
        let route = best.ok_or_else(|| RoutingError::NoPricedRoute {
            from: from.to_string(),
            to: to.to_string(),
            candidates: candidates.len(),
        })?;
        Ok(RouteSelection {
            route,
            candidates_considered: candidates.len(),
            candidates_priced: priced,
        })
    }

    async fn price_path(
        &self,
        providers: &[Arc<dyn LiquidityProvider>],
        assets: &AssetRegistry,
        path: &[(String, String, usize)],
        amount_in: &Decimal,
        dst: &Asset,
    ) -> Option<Route> {
        let mut hops = Vec::with_capacity(path.len());
        let mut running = amount_in.clone();
        for (from, to, idx) in path {
            let from_a = assets.get(from)?;
            let to_a = assets.get(to)?;
            let provider = &providers[*idx];
            let q = match provider.quote(from_a, to_a, &running).await {
                Ok(q) => q,
                Err(e) => {
                    tracing::debug!(provider = provider.id(), %from, %to, error = %e, "leg quote failed; dropping candidate");
                    return None;
                }
            };
            // Providers must answer in the assets' precision; normalise
            // defensively, never in the client's favour.
            let out = q
                .amount_out
                .rescale(to_a.decimals, self.policy.mode(Purpose::QuoteOutput));
            if !out.is_positive() {
                return None;
            }
            let rate = implied_rate(&running, &out, &self.policy).ok()?;
            hops.push(RouteHop {
                provider: provider.id().to_string(),
                from: from_a.clone(),
                to: to_a.clone(),
                amount_in: running.clone(),
                expected_out: out.clone(),
                fee: q.fee,
                rate,
            });
            running = out;
        }
        let extra_hops = hops.len().saturating_sub(1) as u32;
        let penalty = (self.config.hop_penalty_bps * extra_hops).min(BPS_DENOMINATOR);
        let score = running.mul_bps(
            BPS_DENOMINATOR - penalty,
            dst.decimals,
            self.policy.mode(Purpose::QuoteOutput),
        );
        let mut path_codes = vec![path[0].0.clone()];
        path_codes.extend(path.iter().map(|(_, to, _)| to.clone()));
        Some(Route {
            hops,
            path: path_codes,
            amount_in: amount_in.clone(),
            expected_out: running,
            score,
        })
    }

    /// Deterministic ordering: higher score, then fewer hops, then key.
    fn better(a: &Route, b: &Route) -> bool {
        a.score
            .cmp(&b.score)
            .then_with(|| b.hop_count().cmp(&a.hop_count()))
            .then_with(|| b.key().cmp(&a.key()))
            .is_gt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::leg_fees::FeeSchedule;
    use crate::swap::liquidity::StaticRateProvider;

    fn assets() -> AssetRegistry {
        AssetRegistry::new(["USDC", "XLM", "EURC", "BTC", "NGN"].map(Asset::stellar))
    }

    fn router() -> Router {
        Router::new(RouterConfig::default(), RoundingPolicy::STANDARD)
    }

    fn amt(s: &str) -> Decimal {
        Asset::stellar("X").parse_amount(s).unwrap()
    }

    #[tokio::test]
    async fn finds_two_hop_route_when_no_direct_pair_exists() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![
            Arc::new(
                StaticRateProvider::new("lp-a", FeeSchedule::bps(0)).with_rate("USDC", "XLM", "10"),
            ),
            Arc::new(
                StaticRateProvider::new("lp-b", FeeSchedule::bps(0))
                    .with_rate("XLM", "EURC", "0.09"),
            ),
        ];
        let sel = router()
            .best_route(&providers, &assets(), "USDC", "EURC", &amt("100"))
            .await
            .unwrap();
        assert_eq!(sel.route.path, ["USDC", "XLM", "EURC"]);
        assert_eq!(sel.route.key(), "USDC>XLM@lp-a|XLM>EURC@lp-b");
        assert_eq!(sel.route.expected_out.to_string(), "90.0000000");
        assert_eq!(sel.route.hops[1].amount_in.to_string(), "1000.0000000");
    }

    #[tokio::test]
    async fn prefers_better_multi_hop_over_worse_direct() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![
            Arc::new(
                StaticRateProvider::new("lp-a", FeeSchedule::bps(0))
                    .with_rate("USDC", "EURC", "0.80")
                    .with_rate("USDC", "XLM", "10"),
            ),
            Arc::new(
                StaticRateProvider::new("lp-b", FeeSchedule::bps(0))
                    .with_rate("XLM", "EURC", "0.092"),
            ),
        ];
        let sel = router()
            .best_route(&providers, &assets(), "USDC", "EURC", &amt("100"))
            .await
            .unwrap();
        assert_eq!(sel.route.hop_count(), 2);
        assert_eq!(sel.candidates_considered, 2);
        assert_eq!(sel.candidates_priced, 2);
    }

    #[tokio::test]
    async fn hop_penalty_keeps_direct_route_on_near_tie() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![
            Arc::new(
                StaticRateProvider::new("lp-a", FeeSchedule::bps(0))
                    .with_rate("USDC", "EURC", "0.9")
                    .with_rate("USDC", "XLM", "10"),
            ),
            // Two-hop is 4 bps better, less than the 5 bps penalty.
            Arc::new(
                StaticRateProvider::new("lp-b", FeeSchedule::bps(0))
                    .with_rate("XLM", "EURC", "0.090036"),
            ),
        ];
        let sel = router()
            .best_route(&providers, &assets(), "USDC", "EURC", &amt("100"))
            .await
            .unwrap();
        assert_eq!(sel.route.key(), "USDC>EURC@lp-a");
    }

    #[tokio::test]
    async fn equal_scores_tie_break_on_route_key() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![
            Arc::new(
                StaticRateProvider::new("lp-z", FeeSchedule::bps(0)).with_rate("USDC", "XLM", "10"),
            ),
            Arc::new(
                StaticRateProvider::new("lp-a", FeeSchedule::bps(0)).with_rate("USDC", "XLM", "10"),
            ),
        ];
        for _ in 0..3 {
            let sel = router()
                .best_route(&providers, &assets(), "USDC", "XLM", &amt("1"))
                .await
                .unwrap();
            assert_eq!(sel.route.key(), "USDC>XLM@lp-a");
        }
    }

    #[tokio::test]
    async fn respects_max_hops_and_reports_no_path() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![Arc::new(
            StaticRateProvider::new("lp", FeeSchedule::bps(0))
                .with_rate("USDC", "XLM", "10")
                .with_rate("XLM", "BTC", "0.00001")
                .with_rate("BTC", "NGN", "100000000"),
        )];
        let r = Router::new(
            RouterConfig {
                max_hops: 2,
                ..RouterConfig::default()
            },
            RoundingPolicy::STANDARD,
        );
        assert!(matches!(
            r.best_route(&providers, &assets(), "USDC", "NGN", &amt("1"))
                .await,
            Err(RoutingError::NoPath { max_hops: 2, .. })
        ));
        let sel = router()
            .best_route(&providers, &assets(), "USDC", "NGN", &amt("1"))
            .await
            .unwrap();
        assert_eq!(sel.route.path, ["USDC", "XLM", "BTC", "NGN"]);
    }

    #[tokio::test]
    async fn never_revisits_an_asset() {
        // A cycle USDC<->XLM must not produce USDC>XLM>USDC>... paths.
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![Arc::new(
            StaticRateProvider::new("lp", FeeSchedule::bps(0))
                .with_rate("USDC", "XLM", "10")
                .with_rate("XLM", "USDC", "0.1")
                .with_rate("XLM", "EURC", "0.09"),
        )];
        let sel = router()
            .best_route(&providers, &assets(), "USDC", "EURC", &amt("1"))
            .await
            .unwrap();
        assert_eq!(sel.candidates_considered, 1);
    }

    #[tokio::test]
    async fn rejects_unknown_same_and_non_positive() {
        let providers: Vec<Arc<dyn LiquidityProvider>> = vec![];
        let r = router();
        assert_eq!(
            r.best_route(&providers, &assets(), "DOGE", "XLM", &amt("1"))
                .await,
            Err(RoutingError::UnknownAsset("DOGE".into()))
        );
        assert_eq!(
            r.best_route(&providers, &assets(), "XLM", "XLM", &amt("1"))
                .await,
            Err(RoutingError::SameAsset)
        );
        assert_eq!(
            r.best_route(&providers, &assets(), "USDC", "XLM", &amt("0"))
                .await,
            Err(RoutingError::NonPositiveAmount)
        );
    }
}
